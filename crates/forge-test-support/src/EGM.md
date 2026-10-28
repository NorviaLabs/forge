# forge-test-support/src

## Overview

Single-module fixture crate. It provides two families, both dependency-free
beyond `std`: scripted HTTP responses over a real TCP listener, and Git
repository setup helpers.

## Interface

- `git(dir: &Path, args: &[&str])`: runs `git -C <dir> <args>`, panicking with
  the failing argument list if it cannot start or exits non-zero.
- `init_repo(dir: &Path)`: `git init --initial-branch=main -q` plus local
  `user.email`/`user.name`; leaves `HEAD` unborn.
- `init_repo_with_commit(dir: &Path)`: `init_repo` plus a committed `f.txt`, so
  `HEAD` exists.
- `MockResponse = (u16, &'static str, Vec<(&'static str, &'static str)>)`
  (status, body, extra headers).
- `mock_http(responses: Vec<MockResponse>) -> Option<String>`: binds
  `127.0.0.1:0`, serves one response per accepted connection in order, and
  returns the `http://host:port` base URL; returns `None` when the host denies
  binding (treated as "skip this test").

## Invariants

- The listener thread never panics: a malformed or unreadable request closes
  the connection without a response, so a broken test fails client-side rather
  than hanging the process.
- Requests are read to a full header block (plus any `Content-Length` body)
  with a 5-second read timeout and a 64 KiB header cap; connections are closed
  after each response.
- Responses carry `content-length`, caller-supplied headers, and
  `connection: close`.
- The thread exits when the response list is exhausted or `accept` fails; it is
  detached, not joined by the caller.
- `init_repo*` are idempotent only in the sense that they create a fresh repo;
  calling them on an existing repo re-runs the config commands.
- `git` panics instead of returning a `Result`: these are fixtures, and a
  silent failure would surface as a confusing assertion elsewhere.

## Side Effects

- Runs the `git` binary as a child process (`-C <dir>`), requiring it on
  `PATH`.
- Writes `f.txt` and repository metadata into the supplied directory.
- Binds a TCP port and spawns a detached listener thread that remains alive
  until its responses are consumed or the process exits.
- Prints a skip notice to stderr when binding is denied.

## Verification

- Run: `cargo test --package forge-test-support --locked`
