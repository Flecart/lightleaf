//! In-memory job records. The service keeps no persistent state: after a restart, the only
//! trace of a job is its commit in the project's history.

use super::now;
use serde::Serialize;
use serde_json::Value;
use std::{collections::HashMap, sync::Mutex};

/// Finished jobs are forgotten after this long.
const RETENTION_SECS: u64 = 86_400;
const MAX_JOBS: usize = 1000;

#[derive(Serialize, Clone, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Running,
    Done,
    Failed,
}

#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub project_id: String,
    pub kind: String,
    pub status: Status,
    pub stage: String,
    pub created_at: u64,
    pub finished_at: Option<u64>,
    pub error: Option<String>,
    pub result: Option<Value>,
}

impl Job {
    fn active(&self) -> bool {
        matches!(self.status, Status::Queued | Status::Running)
    }
}

#[derive(Default)]
pub struct JobStore {
    jobs: Mutex<HashMap<String, Job>>,
}

impl JobStore {
    /// Registers a new queued job, unless the project already has one queued or running.
    pub fn create(&self, project_id: &str, kind: &str) -> Result<Job, &'static str> {
        let mut jobs = self.jobs.lock().unwrap();
        prune(&mut jobs);
        if jobs
            .values()
            .any(|j| j.project_id == project_id && j.active())
        {
            return Err("a review job is already running for this project");
        }
        let job = Job {
            id: uuid::Uuid::new_v4().to_string(),
            project_id: project_id.to_string(),
            kind: kind.to_string(),
            status: Status::Queued,
            stage: "Waiting for a free slot".into(),
            created_at: now(),
            finished_at: None,
            error: None,
            result: None,
        };
        jobs.insert(job.id.clone(), job.clone());
        Ok(job)
    }

    pub fn set_stage(&self, id: &str, stage: String) {
        self.update(id, |j| j.stage = stage);
    }

    pub fn start(&self, id: &str) {
        self.update(id, |j| {
            j.status = Status::Running;
            j.stage = "Starting".into();
        });
    }

    pub fn finish(&self, id: &str, outcome: anyhow::Result<Value>) {
        self.update(id, |j| {
            j.finished_at = Some(now());
            match outcome {
                Ok(result) => {
                    j.status = Status::Done;
                    j.stage = "Finished".into();
                    j.result = Some(result);
                }
                Err(e) => {
                    tracing::warn!("job {} failed: {e:#}", j.id);
                    j.status = Status::Failed;
                    j.stage = "Failed".into();
                    j.error = Some(format!("{e:#}"));
                }
            }
        });
    }

    fn update(&self, id: &str, f: impl FnOnce(&mut Job)) {
        if let Some(j) = self.jobs.lock().unwrap().get_mut(id) {
            f(j);
        }
    }

    /// The project's most recent job.
    pub fn latest(&self, project_id: &str) -> Option<Job> {
        let jobs = self.jobs.lock().unwrap();
        jobs.values()
            .filter(|j| j.project_id == project_id)
            .max_by_key(|j| j.created_at)
            .cloned()
    }

    /// One job, only if it belongs to `project_id`.
    pub fn get(&self, project_id: &str, id: &str) -> Option<Job> {
        self.jobs
            .lock()
            .unwrap()
            .get(id)
            .filter(|j| j.project_id == project_id)
            .cloned()
    }

    /// (running, queued)
    pub fn counts(&self) -> (usize, usize) {
        let jobs = self.jobs.lock().unwrap();
        let n = |s: Status| jobs.values().filter(|j| j.status == s).count();
        (n(Status::Running), n(Status::Queued))
    }
}

fn prune(jobs: &mut HashMap<String, Job>) {
    let cutoff = now().saturating_sub(RETENTION_SECS);
    jobs.retain(|_, j| j.finished_at.is_none_or(|t| t > cutoff));
    if jobs.len() > MAX_JOBS {
        let mut finished: Vec<(u64, String)> = jobs
            .values()
            .filter_map(|j| j.finished_at.map(|t| (t, j.id.clone())))
            .collect();
        finished.sort();
        let excess = jobs.len() - MAX_JOBS;
        for (_, id) in finished.into_iter().take(excess) {
            jobs.remove(&id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_active_job_per_project() {
        let s = JobStore::default();
        let a = s.create("p1", "review").unwrap();
        assert!(s.create("p1", "clear").is_err());
        assert!(s.create("p2", "review").is_ok());
        s.start(&a.id);
        assert_eq!(s.counts(), (1, 1));
        s.finish(&a.id, Ok(Value::Null));
        assert!(s.create("p1", "clear").is_ok());
        assert!(s.get("p2", &a.id).is_none());
        assert_eq!(s.get("p1", &a.id).unwrap().status, Status::Done);
    }
}
