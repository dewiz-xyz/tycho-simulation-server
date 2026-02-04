use std::env;

use jemalloc_ctl::{arenas, raw};
use tracing::{info, warn};

pub fn maybe_purge_allocator(context: &'static str) {
    let enabled = env::var("STREAM_MEM_PURGE")
        .map(|value| value == "1")
        .unwrap_or(false);
    if !enabled {
        return;
    }

    purge_jemalloc(context);
}

fn purge_jemalloc(context: &'static str) {
    let arenas = match arenas::narenas::read() {
        Ok(count) => count as usize,
        Err(err) => {
            warn!(
                event = "memory_purge",
                context,
                method = "jemalloc",
                error = %err,
                "Failed reading jemalloc arena count"
            );
            return;
        }
    };

    let mut purged = 0usize;
    let mut failed = 0usize;
    for arena in 0..arenas {
        let name = format!("arena.{}.purge\0", arena);
        let result = unsafe { raw::write(name.as_bytes(), ()) };
        match result {
            Ok(()) => purged += 1,
            Err(err) => {
                failed += 1;
                warn!(
                    event = "memory_purge",
                    context,
                    method = "jemalloc",
                    arena,
                    error = %err,
                    "Failed to purge jemalloc arena"
                );
            }
        }
    }

    info!(
        event = "memory_purge",
        context,
        method = "jemalloc",
        arenas,
        purged,
        failed,
        "jemalloc purge complete"
    );
}
