//! Pod stop and restart: group-kill every world pod and remove its tmpfs bundle.

use std::fs;
use std::sync::Arc;

use slipfork_world::kill_group;

use super::InitWorld;
use super::launch::{crun_delete, crun_run};
use crate::shared::reap_zombies;
use crate::{Fault, REMEDY_RUNTIME};

impl InitWorld {
    /// Restarts one pod or every pod from its existing tmpfs bundle.
    pub(crate) fn restart(&mut self, service: Option<&str>) -> Result<usize, Fault> {
        let ids: Vec<String> = {
            let pids = self
                .shared
                .pids
                .lock()
                .map_err(|_| Fault::new("pod pid lock", REMEDY_RUNTIME))?;
            match service {
                Some(id) if pids.contains_key(id) => vec![id.to_owned()],
                Some(id) => {
                    return Err(Fault::new(
                        format!("pod `{id}` is not running"),
                        "pick a service from `slipfork up` output or omit the service name",
                    ));
                }
                None => pids.keys().cloned().collect(),
            }
        };
        for id in &ids {
            let pid = self
                .shared
                .pids
                .lock()
                .ok()
                .and_then(|pids| pids.get(id).copied());
            let sink = self
                .shared
                .logs
                .lock()
                .ok()
                .and_then(|logs| logs.get(id).cloned());
            let (Some(pid), Some(info), Some(sink)) = (pid, self.shared.info(id), sink) else {
                continue;
            };
            kill_group(pid);
            crun_delete(&self.shared, id);
            {
                let mut guard = sink
                    .lock()
                    .map_err(|_| Fault::new(format!("pod `{id}` log lock"), REMEDY_RUNTIME))?;
                guard.divider(id, true).map_err(|source| {
                    Fault::cause(
                        format!("pod `{id}` restart divider"),
                        REMEDY_RUNTIME,
                        source,
                    )
                })?;
            }
            let pid = crun_run(&self.shared, &info.bundle, id, Arc::clone(&sink))?;
            if let Ok(mut pids) = self.shared.pids.lock() {
                pids.insert(id.clone(), pid);
            }
            if let Ok(mut pods) = self.shared.pods.lock()
                && let Some(row) = pods.get_mut(id)
            {
                row.launches += 1;
            }
        }
        reap_zombies(&self.shared);
        Ok(ids.len())
    }

    pub(crate) fn stop_all(&mut self) {
        self.stop_s3();
        self.shared.mark_stopped();
        for pid in self.shared.exec_pids() {
            kill_group(pid);
        }
        let pids = self
            .shared
            .pids
            .lock()
            .map(|mut guard| std::mem::take(&mut *guard))
            .unwrap_or_default();
        for (id, pid) in pids {
            crun_delete(&self.shared, &id);
            kill_group(pid);
            if let Ok(mut pods) = self.shared.pods.lock()
                && let Some(container) = pods.remove(&id)
            {
                let _ = fs::remove_dir_all(&container.bundle);
            }
        }
        if let Ok(mut logs) = self.shared.logs.lock() {
            logs.clear();
        }
        reap_zombies(&self.shared);
    }
}
