use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::core::graph_index::{self, ProjectIndex};
use crate::core::vector_index::BM25Index;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Idle,
    Building,
    Ready,
    Failed,
}

#[derive(Debug, Clone)]
struct Component {
    state: State,
    started_ms: Option<u64>,
    finished_ms: Option<u64>,
    duration_ms: Option<u64>,
    last_error: Option<String>,
}

impl Component {
    fn new() -> Self {
        Self {
            state: State::Idle,
            started_ms: None,
            finished_ms: None,
            duration_ms: None,
            last_error: None,
        }
    }
}

#[derive(Debug)]
struct ProjectBuild {
    worker_running: bool,
    graph: Component,
    bm25: Component,
}

impl ProjectBuild {
    fn new() -> Self {
        Self {
            worker_running: false,
            graph: Component::new(),
            bm25: Component::new(),
        }
    }
}

static REGISTRY: OnceLock<Mutex<HashMap<String, Arc<Mutex<ProjectBuild>>>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, Arc<Mutex<ProjectBuild>>>> {
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

fn entry_for(project_root: &str) -> Arc<Mutex<ProjectBuild>> {
    let mut map = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    map.entry(project_root.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(ProjectBuild::new())))
        .clone()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn start_component(c: &mut Component) {
    c.state = State::Building;
    c.started_ms = Some(now_ms());
    c.finished_ms = None;
    c.duration_ms = None;
    c.last_error = None;
}

fn finish_ok(c: &mut Component) {
    c.state = State::Ready;
    let end = now_ms();
    c.finished_ms = Some(end);
    c.duration_ms = c.started_ms.map(|s| end.saturating_sub(s));
}

fn finish_err(c: &mut Component, e: String) {
    c.state = State::Failed;
    let end = now_ms();
    c.finished_ms = Some(end);
    c.duration_ms = c.started_ms.map(|s| end.saturating_sub(s));
    c.last_error = Some(e);
}

pub fn ensure_all_background(project_root: &str) {
    let state = entry_for(project_root);
    let should_spawn = {
        let mut s = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if s.worker_running {
            false
        } else {
            s.worker_running = true;
            true
        }
    };

    if !should_spawn {
        return;
    }

    let root = project_root.to_string();
    std::thread::spawn(move || {
        let state = entry_for(&root);

        {
            let mut s = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            start_component(&mut s.graph);
        }
        let idx = std::panic::catch_unwind(|| {
            let idx = graph_index::load_or_build(&root);
            let _ = idx.save();
            idx
        });
        if let Ok(_i) = idx {
            let mut s = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            finish_ok(&mut s.graph);
        } else {
            let mut s = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            finish_err(&mut s.graph, "graph index build panicked".to_string());
        }

        {
            let mut s = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            start_component(&mut s.bm25);
        }
        let bm = std::panic::catch_unwind(|| {
            let root_pb = Path::new(&root);
            let idx = BM25Index::load_or_build(root_pb);
            let _ = idx.save(root_pb);
        });
        if let Ok(()) = bm {
            let mut s = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            finish_ok(&mut s.bm25);
        } else {
            let mut s = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            finish_err(&mut s.bm25, "bm25 build panicked".to_string());
        }

        let mut s = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        s.worker_running = false;
    });
}

pub fn try_load_graph_index(project_root: &str) -> Option<ProjectIndex> {
    ProjectIndex::load(project_root).filter(|idx| !idx.files.is_empty())
}

pub fn try_load_bm25_index(project_root: &str) -> Option<BM25Index> {
    BM25Index::load(Path::new(project_root))
}

#[derive(Debug, Serialize)]
struct ComponentStatus<'a> {
    state: &'a str,
    started_ms: Option<u64>,
    finished_ms: Option<u64>,
    duration_ms: Option<u64>,
    last_error: Option<&'a str>,
}

fn component_status(c: &Component) -> ComponentStatus<'_> {
    ComponentStatus {
        state: match c.state {
            State::Idle => "idle",
            State::Building => "building",
            State::Ready => "ready",
            State::Failed => "failed",
        },
        started_ms: c.started_ms,
        finished_ms: c.finished_ms,
        duration_ms: c.duration_ms,
        last_error: c.last_error.as_deref(),
    }
}

#[derive(Debug, Serialize)]
struct StatusResponse<'a> {
    project_root: &'a str,
    graph_index: ComponentStatus<'a>,
    bm25_index: ComponentStatus<'a>,
}

pub fn status_json(project_root: &str) -> String {
    let state = entry_for(project_root);
    let s = state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let res = StatusResponse {
        project_root,
        graph_index: component_status(&s.graph),
        bm25_index: component_status(&s.bm25),
    };
    serde_json::to_string(&res).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_json_is_valid_json() {
        let s = status_json("/tmp");
        let _: serde_json::Value = serde_json::from_str(&s).unwrap();
    }
}
