note we have the full rust analyzer workspace in `tmp/rust-analyzer` for your reference and perusal. TAKE ADVANTAGE OF THIS WHENEVER RUST ANALYZER FEATURES WOULD HELP OUR LOGIC.

IMPORTANT: we don't give a flying fuck about backwards compatibilty. this is literally an unpublished local crate. we care much more about long term maintainability and keeping the right things in the code.

Any supported RAQL query execution path must go through the daemon-backed, incremental rust-analyzer runtime. Any direct runtime path is dev-only, quarantined, and must not be wired into public CLI behavior.
