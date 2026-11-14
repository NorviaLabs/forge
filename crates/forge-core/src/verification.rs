//! Recognising verification commands: the tests, type checks, linters and
//! builds an agent runs to check its own work.
//!
//! This only labels a command for the turn record's signals
//! (`verification_passed` / `verification_failed`). It never changes how a
//! turn is judged — that stays with the completion evaluator.
//!
//! A command counts when it contains a project's configured validation
//! command (`[validation] command` in `forge.toml`), or when any segment of
//! it (split on `&&`, `||`, `;`, `|` and newlines) runs a well-known test,
//! check, lint or build entry point.

/// Whether `command` runs a verification step. `configured` holds the
/// project's own validation command lines.
pub(crate) fn is_verification_command(command: &str, configured: &[String]) -> bool {
    if configured
        .iter()
        .map(|line| line.trim())
        .any(|line| !line.is_empty() && command.contains(line))
    {
        return true;
    }
    segments(command).any(|segment| segment_verifies(&segment))
}

fn segments(command: &str) -> impl Iterator<Item = Vec<String>> + '_ {
    command
        .split(['\n', ';', '|', '&'])
        .map(|segment| {
            segment
                .split_whitespace()
                .map(|token| token.trim_matches(|c| matches!(c, '"' | '\'' | '(' | ')')))
                .filter(|token| !token.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .filter(|tokens| !tokens.is_empty())
}

/// Drop what only wraps the real program: environment assignments and
/// runners such as `timeout 60`, `env`, `uv run`, `npx`.
fn strip_wrappers(tokens: &[String]) -> &[String] {
    let mut rest = tokens;
    loop {
        let Some(first) = rest.first() else {
            return rest;
        };
        let program = basename(first);
        let is_assignment = first.split_once('=').is_some_and(|(name, _)| {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        });
        rest = if is_assignment || matches!(program, "env" | "time" | "nice" | "xvfb-run" | "cd") {
            // `cd dir` never verifies on its own; skip it and its argument.
            if program == "cd" {
                rest.get(2..).unwrap_or_default()
            } else {
                &rest[1..]
            }
        } else if program == "timeout" {
            // `timeout [-s SIG] [-k DUR] DURATION cmd…`: the command starts
            // after the first bare duration.
            match rest
                .iter()
                .enumerate()
                .skip(1)
                .find(|(index, token)| is_duration(token) && !is_option_value(rest, *index))
            {
                Some((index, _)) => &rest[index + 1..],
                None => &[],
            }
        } else if matches!(program, "npx" | "pnpx" | "bunx") {
            &rest[1..]
        } else if matches!(
            (program, rest.get(1).map(String::as_str)),
            (
                "uv" | "poetry" | "pipenv" | "hatch" | "pdm" | "rye",
                Some("run")
            ) | ("bundle", Some("exec"))
        ) {
            // `uv run pytest`, `bundle exec rspec`.
            &rest[2..]
        } else {
            return rest;
        };
    }
}

/// `60`, `1.5`, `30s`, `5m`, `1h`, `2d`.
fn is_duration(token: &str) -> bool {
    let number = token.trim_end_matches(['s', 'm', 'h', 'd']);
    !number.is_empty() && number.parse::<f64>().is_ok()
}

/// Whether `tokens[index]` is the value of the option just before it
/// (`-k 5`, `--kill-after 5`, `-s 9`).
fn is_option_value(tokens: &[String], index: usize) -> bool {
    index >= 2
        && matches!(
            tokens[index - 1].as_str(),
            "-k" | "--kill-after" | "-s" | "--signal"
        )
}

fn basename(token: &str) -> &str {
    token.rsplit('/').next().unwrap_or(token)
}

/// A script or target name that verifies: `test`, `test:unit`, `lint`,
/// `typecheck`, `check-all`, `build`, `ci`, `verify`.
fn verifying_name(name: &str) -> bool {
    const NAMES: &[&str] = &[
        "test",
        "tests",
        "lint",
        "check",
        "typecheck",
        "type-check",
        "tsc",
        "build",
        "ci",
        "verify",
        "spec",
    ];
    let name = name.to_ascii_lowercase();
    NAMES.iter().any(|known| {
        name == *known
            || name
                .strip_prefix(known)
                .is_some_and(|rest| rest.starts_with([':', '-', '_', '.']))
    })
}

fn segment_verifies(tokens: &[String]) -> bool {
    let tokens = strip_wrappers(tokens);
    let Some(first) = tokens.first() else {
        return false;
    };
    let program = basename(first);
    let args: Vec<&str> = tokens[1..].iter().map(String::as_str).collect();
    let sub = args.iter().copied().find(|arg| !arg.starts_with('-'));
    let has_flag = |flag: &str| args.contains(&flag);
    match program {
        "cargo" => sub.is_some_and(|sub| {
            matches!(
                sub,
                "test" | "t" | "check" | "c" | "clippy" | "nextest" | "build" | "b" | "miri"
            ) || (sub == "fmt" && has_flag("--check"))
        }),
        "npm" | "pnpm" | "yarn" | "bun" => match sub {
            Some("run" | "run-script") => args
                .iter()
                .copied()
                .skip_while(|arg| !matches!(*arg, "run" | "run-script"))
                .nth(1)
                .is_some_and(verifying_name),
            Some("t" | "tst") => true,
            // `pnpm exec vitest`, `yarn dlx tsc`: the tool that follows.
            Some("exec" | "dlx" | "x") => args
                .iter()
                .position(|arg| matches!(*arg, "exec" | "dlx" | "x"))
                .is_some_and(|at| segment_verifies(&tokens[at + 2..])),
            Some(script) => {
                verifying_name(script)
                    || matches!(
                        script,
                        "vitest" | "jest" | "tsc" | "vue-tsc" | "eslint" | "mocha" | "playwright"
                    )
            }
            None => false,
        },
        "pytest" | "py.test" | "tox" | "nox" | "mypy" | "pyright" | "flake8" | "pylint" | "tsc"
        | "vue-tsc" | "vitest" | "jest" | "mocha" | "ava" | "eslint" | "rspec" | "phpunit"
        | "ctest" | "shellcheck" | "golangci-lint" | "swiftlint" | "ktlint" | "hadolint"
        | "stylelint" => true,
        "ruff" => sub != Some("format") || has_flag("--check"),
        "black" | "prettier" | "gofmt" | "rustfmt" => {
            has_flag("--check") || has_flag("-l") || has_flag("--list-different")
        }
        "biome" => sub.is_some_and(|sub| matches!(sub, "check" | "ci" | "lint")),
        "playwright" => sub == Some("test"),
        "cypress" => sub == Some("run"),
        "python" | "python3" | "py" => {
            args.first() == Some(&"-m")
                && args.get(1).is_some_and(|module| {
                    matches!(
                        *module,
                        "pytest" | "unittest" | "mypy" | "tox" | "ruff" | "pyright" | "pylint"
                    )
                })
        }
        "go" => sub.is_some_and(|sub| matches!(sub, "test" | "vet" | "build")),
        "make" | "just" | "task" | "mage" => sub.is_some_and(verifying_name),
        "mvn" | "mvnw" => sub.is_some_and(|sub| matches!(sub, "test" | "verify")),
        "gradle" | "gradlew" => sub.is_some_and(|sub| matches!(sub, "test" | "check" | "build")),
        "dotnet" | "swift" => sub.is_some_and(|sub| matches!(sub, "test" | "build")),
        "mix" => sub.is_some_and(|sub| matches!(sub, "test" | "credo" | "dialyzer")),
        "rake" => sub.is_some_and(|sub| matches!(sub, "test" | "spec")),
        "deno" => sub.is_some_and(|sub| matches!(sub, "test" | "check" | "lint")),
        "bazel" | "bazelisk" => sub.is_some_and(|sub| matches!(sub, "test" | "build")),
        "zig" => sub.is_some_and(|sub| sub == "test") || args.starts_with(&["build", "test"]),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::is_verification_command;

    fn verifies(command: &str) -> bool {
        is_verification_command(command, &[])
    }

    #[test]
    fn common_test_check_lint_and_build_commands_verify() {
        for command in [
            "cargo test",
            "cargo test -p forge-core -- --nocapture",
            "cargo clippy --all-targets -- -D warnings",
            "cargo fmt --check",
            "cargo nextest run",
            "npm test",
            "npm run test:unit",
            "pnpm run lint",
            "pnpm typecheck",
            "yarn test --watch=false",
            "bun test",
            "npx tsc --noEmit",
            "npx vitest run",
            "pytest -q tests/test_api.py",
            "python -m pytest",
            "python3 -m unittest discover",
            "uv run pytest",
            "poetry run mypy src",
            "ruff check .",
            "go test ./...",
            "go vet ./...",
            "make test",
            "make check-all",
            "./gradlew test",
            "mvn verify",
            "dotnet test",
            "bundle exec rspec spec/models",
            "deno test",
            "tsc -p .",
            "golangci-lint run",
        ] {
            assert!(verifies(command), "{command} should verify");
        }
    }

    #[test]
    fn wrappers_chains_and_environment_are_seen_through() {
        for command in [
            "cd crates/forge-core && cargo test",
            "RUST_LOG=debug cargo test",
            "timeout 300 cargo test",
            "timeout -s KILL 60 npm test",
            "env CI=1 npm test",
            "git status; cargo check",
            "cargo build 2>&1 | tail -20",
            "(cd web && pnpm run build)",
            "timeout --signal KILL 2m pnpm exec vitest run",
            "pnpm vitest",
            "ruff format --check .",
        ] {
            assert!(verifies(command), "{command} should verify");
        }
    }

    #[test]
    fn ordinary_commands_do_not_verify() {
        for command in [
            "ls -la",
            "cat src/lib.rs",
            "rg TODO",
            "git diff",
            "cargo add serde",
            "cargo run -- --help",
            "npm install",
            "npm run dev",
            "pnpm run format",
            "python script.py",
            "go run ./cmd/server",
            "make install",
            "cargo fmt",
            "prettier --write .",
            "ruff format .",
            "timeout 5 sleep 1",
            "echo test",
            "",
        ] {
            assert!(!verifies(command), "{command} should not verify");
        }
    }

    #[test]
    fn the_projects_configured_validation_command_verifies() {
        let configured = vec!["./scripts/validate.sh --fast".to_string(), "  ".to_string()];
        assert!(is_verification_command(
            "./scripts/validate.sh --fast",
            &configured
        ));
        assert!(is_verification_command(
            "cd repo && ./scripts/validate.sh --fast 2>&1",
            &configured
        ));
        assert!(!is_verification_command("./scripts/deploy.sh", &configured));
        // A blank configured line never matches everything.
        assert!(!is_verification_command("ls", &configured));
    }
}
