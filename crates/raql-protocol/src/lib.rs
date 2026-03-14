#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DaemonRequest {
    Run(RunRequest),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunRequest {
    pub protocol_version: u32,
    pub program_path: String,
    pub include_dirs: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum DaemonEvent {
    Session(SessionEvent),
    Plan(PlanSummary),
    Result(RunResult),
    Error(ErrorEvent),
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionEvent {
    pub workspace_root: String,
    pub daemon_state: DaemonState,
    pub workspace_epoch: u64,
    pub content_revision: u64,
    pub supported_capabilities: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum DaemonState {
    Cold,
    Warm,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanSummary {
    pub program_path: String,
    pub predicates: usize,
    pub facts: usize,
    pub rules: usize,
    pub strata: usize,
    pub sccs: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunResult {
    pub status: String,
    pub iterations: usize,
    pub notes: Vec<RunNote>,
    pub relations: Vec<RelationRows>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunNote {
    pub section: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelationRows {
    pub name: String,
    pub rows: Vec<Vec<ProtocolValue>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ProtocolValue {
    Int(i64),
    String(String),
    Bool(bool),
    Enum { name: String, variant: String },
    Host { kind: String, id: u64 },
    None,
    Some(Box<ProtocolValue>),
    List(Vec<ProtocolValue>),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorEvent {
    pub message: String,
}
