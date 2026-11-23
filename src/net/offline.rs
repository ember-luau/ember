/*! `--offline`, or `EMBER_OFFLINE=1`: the run downloads nothing. one switch
for the process, which the HTTP agents, the git fetches and the npm install
each ask before they reach for the network. it is off until [`enable`] and never goes off
again, since a run that was told to stay offline has no step that may go
online. */

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};

static OFFLINE: AtomicBool = AtomicBool::new(false);

/// what an HTTP request fails with while offline. the caller's error names the URL.
pub const REASON: &str = "embr is offline (--offline), and this is not in a cache";

pub fn enable() {
    OFFLINE.store(true, Ordering::Relaxed);
}

pub fn is_enabled() -> bool {
    OFFLINE.load(Ordering::Relaxed)
}

/** the name lookup of every HTTP agent. each request starts here, so this
is the one place that stops them all, whichever function made the request.
it asks the switch at each lookup, because the agents are built once and
may be older than the switch. */
pub fn resolve(address: &str) -> std::io::Result<Vec<SocketAddr>> {
    resolve_when(is_enabled(), address)
}

fn resolve_when(offline: bool, address: &str) -> std::io::Result<Vec<SocketAddr>> {
    if offline {
        return Err(std::io::Error::other(REASON));
    }
    std::net::ToSocketAddrs::to_socket_addrs(address).map(Iterator::collect)
}

#[cfg(test)]
mod tests {
    use super::*;

    /* the switch itself stays off in the tests: it holds for the whole
    process, and the other tests share that process */
    #[test]
    fn offline_resolves_no_name() {
        let error = resolve_when(true, "127.0.0.1:80").unwrap_err();
        assert!(error.to_string().contains("--offline"), "{error}");
        assert_eq!(resolve_when(false, "127.0.0.1:80").unwrap().len(), 1);
    }
}
