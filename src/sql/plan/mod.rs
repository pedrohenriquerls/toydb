mod optimizer;
mod planner;
use optimizer::Optimizer as _;
use planner::Planner;

use super::engine::Transaction;
use super::execution::{Context, Executor, ResultSet};
use super::parser::ast;
use super::schema::Table;
use super::types::{Expression, Expressions};
use crate::Error;

use std::collections::BTreeMap;

/// A query plan
#[derive(Debug)]
pub struct Plan(Node);

impl Plan {
    /// Builds a plan from an AST statement
    pub fn build(statement: ast::Statement) -> Result<Self, Error> {
        Planner::new().build(statement)
    }

    /// Executes the plan, consuming it
    pub fn execute<T: Transaction + 'static>(
        self,
        mut ctx: Context<T>,
    ) -> Result<ResultSet, Error> {
        Executor::build(self.0).execute(&mut ctx)
    }

    /// Optimizes the plan, consuming it
    pub fn optimize(self) -> Result<Self, Error> {
        Ok(Plan(optimizer::ConstantFolder.optimize(self.0)?))
    }
}

/// A plan node
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum Node {
    Aggregation {
        source: Box<Node>,
        aggregates: Vec<Aggregate>,
    },
    CreateTable {
        schema: Table,
    },
    Delete {
        table: String,
        source: Box<Node>,
    },
    DropTable {
        name: String,
    },
    Explain(Box<Node>),
    Filter {
        source: Box<Node>,
        predicate: Expression,
    },
    Insert {
        table: String,
        columns: Vec<String>,
        expressions: Vec<Expressions>,
    },
    Limit {
        source: Box<Node>,
        limit: u64,
    },
    NestedLoopJoin {
        outer: Box<Node>,
        inner: Box<Node>,
        predicate: Option<Expression>,
        pad: bool,
        flip: bool,
    },
    Nothing,
    Offset {
        source: Box<Node>,
        offset: u64,
    },
    Order {
        source: Box<Node>,
        orders: Vec<(Expression, Direction)>,
    },
    Projection {
        source: Box<Node>,
        labels: Vec<Option<String>>,
        expressions: Expressions,
    },
    Scan {
        table: String,
        alias: Option<String>,
    },
    // Uses BTreeMap for test stability
    Update {
        table: String,
        source: Box<Node>,
        expressions: BTreeMap<String, Expression>,
    },
}

impl Node {
    /// Recursively transforms nodes by applying functions before and after descending.
    pub fn transform<B, A>(mut self, pre: &B, post: &A) -> Result<Self, Error>
    where
        B: Fn(Self) -> Result<Self, Error>,
        A: Fn(Self) -> Result<Self, Error>,
    {
        self = pre(self)?;
        self = match self {
            n @ Self::CreateTable { .. } => n,
            n @ Self::DropTable { .. } => n,
            n @ Self::Insert { .. } => n,
            n @ Self::Nothing => n,
            n @ Self::Scan { .. } => n,
            Self::Aggregation { source, aggregates } => {
                Self::Aggregation { source: source.transform(pre, post)?.into(), aggregates }
            }
            Self::Delete { table, source } => {
                Self::Delete { table, source: source.transform(pre, post)?.into() }
            }
            Self::Explain(node) => Self::Explain(node.transform(pre, post)?.into()),
            Self::Filter { source, predicate } => {
                Self::Filter { source: source.transform(pre, post)?.into(), predicate }
            }
            Self::Limit { source, limit } => {
                Self::Limit { source: source.transform(pre, post)?.into(), limit }
            }
            Self::NestedLoopJoin { outer, inner, predicate, pad, flip } => Self::NestedLoopJoin {
                outer: outer.transform(pre, post)?.into(),
                inner: inner.transform(pre, post)?.into(),
                predicate,
                pad,
                flip,
            },
            Self::Offset { source, offset } => {
                Self::Offset { source: source.transform(pre, post)?.into(), offset }
            }
            Self::Order { source, orders } => {
                Self::Order { source: source.transform(pre, post)?.into(), orders }
            }
            Self::Projection { source, labels, expressions } => Self::Projection {
                source: source.transform(pre, post)?.into(),
                labels,
                expressions,
            },
            Self::Update { table, source, expressions } => {
                Self::Update { table, source: source.transform(pre, post)?.into(), expressions }
            }
        };
        post(self)
    }

    /// Transforms all expressions in a node by calling .transform() on them
    /// with the given functions.
    pub fn transform_expressions<B, A>(self, pre: &B, post: &A) -> Result<Self, Error>
    where
        B: Fn(Expression) -> Result<Expression, Error>,
        A: Fn(Expression) -> Result<Expression, Error>,
    {
        Ok(match self {
            n @ Self::Aggregation { .. } => n,
            n @ Self::CreateTable { .. } => n,
            n @ Self::Delete { .. } => n,
            n @ Self::DropTable { .. } => n,
            n @ Self::Explain { .. } => n,
            n @ Self::Limit { .. } => n,
            n @ Self::NestedLoopJoin { .. } => n,
            n @ Self::Nothing => n,
            n @ Self::Offset { .. } => n,
            n @ Self::Scan { .. } => n,
            Self::Filter { source, predicate } => {
                Self::Filter { source, predicate: predicate.transform(pre, post)? }
            }
            Self::Insert { table, columns, expressions } => Self::Insert {
                table,
                columns,
                expressions: expressions
                    .into_iter()
                    .map(|exprs| exprs.into_iter().map(|e| e.transform(pre, post)).collect())
                    .collect::<Result<_, Error>>()?,
            },
            Self::Order { source, orders } => Self::Order {
                source,
                orders: orders
                    .into_iter()
                    .map(|(e, o)| e.transform(pre, post).map(|e| (e, o)))
                    .collect::<Result<_, Error>>()?,
            },
            Self::Projection { source, labels, expressions } => Self::Projection {
                source,
                labels,
                expressions: expressions
                    .into_iter()
                    .map(|e| e.transform(pre, post))
                    .collect::<Result<_, Error>>()?,
            },
            Self::Update { table, source, expressions } => Self::Update {
                table,
                source,
                expressions: expressions
                    .into_iter()
                    .map(|(k, e)| e.transform(pre, post).map(|e| (k, e)))
                    .collect::<Result<_, Error>>()?,
            },
        })
    }
}

/// An aggregate operation
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum Aggregate {
    Average,
    Count,
    Max,
    Min,
    Sum,
}

pub type Aggregates = Vec<Aggregate>;

/// A sort order direction
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub enum Direction {
    Ascending,
    Descending,
}

impl From<ast::Order> for Direction {
    fn from(order: ast::Order) -> Self {
        match order {
            ast::Order::Ascending => Self::Ascending,
            ast::Order::Descending => Self::Descending,
        }
    }
}
