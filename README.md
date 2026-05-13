# thermite

**thermite is beta software.  Please use it with caution and report any bugs you discover.**

thermite is a Ubuntu Linux command-line tool that automates packaging the upstream Rust
toolchain into versioned Ubuntu `.deb` source packages.  It implements the two primary
workflows used by Ubuntu Foundations toolchain maintainers:

- **`update`** — create a new versioned `rustc-X.Y` source package for a new upstream
  Rust release (e.g. `rustc-1.85`).
- **`backport`** *(planned)* — adapt an existing versioned `rustc-X.Y` package for an
  older Ubuntu Long-Term Support (LTS) release.

Reference workflows:
- Update: <https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/update-rust/>
- Backport: <https://documentation.ubuntu.com/project/maintainers/niche-package-maintenance/rustc/backport-rust/>

---

## Overview

Every new upstream Rust release requires a new versioned source package (e.g. `rustc-1.85`)
to be created in the Ubuntu archive.  The process involves many ordered steps: fetching the
upstream source with `uscan`, refreshing Debian patches with `quilt`, pruning unwanted
vendored dependencies, updating control files, running local builds with `sbuild`, checking
the result with `lintian`, uploading to a PPA, running `autopkgtest`s, and finally requesting
sponsorship.

thermite drives the entire sequence, fully automating what can be automated and pausing at
checkpoints that require human judgment (patch conflicts, copyright stanza authoring, build
failure triage, etc.).

---

## Prerequisites

The following tools must be installed and available on `PATH` before running thermite.
On Ubuntu, most are available through the standard package repositories or as snaps.

| Tool | Ubuntu package / snap | Purpose |
|------|-----------------------|---------|
| `git` | `git` | Version control |
| `dch` | `devscripts` | Debian changelog editing |
| `uscan` | `devscripts` | Upstream source download |
| `gbp` | `git-buildpackage` | Import upstream source into Git |
| `quilt` | `quilt` | Patch management |
| `dpkg-buildpackage` | `dpkg-dev` | Source package building |
| `sbuild` | `sbuild` | Clean-room binary package builds |
| `lintian` | `lintian` | Debian policy compliance checks |
| `cargo` | `rustup` snap | Rust build tool (for vendor pruning) |
| `rustup` | `rustup` snap | Rust toolchain manager |
| `dput` | `dput` | Upload packages to Launchpad PPAs |
| `ppa` | `ppa-dev-tools` snap | Create and manage Launchpad PPAs |
| `autopkgtest` | `autopkgtest` | Installed-package test runner |
| `autopkgtest-buildvm-ubuntu-cloud` | `autopkgtest` | Create QEMU autopkgtest images |

Install the snaps:

```sh
sudo snap install rustup --classic
sudo snap install ppa-dev-tools
```

Install the apt packages:

```sh
sudo apt install git devscripts git-buildpackage quilt dpkg-dev \
    sbuild lintian dput autopkgtest
```

---

## Installation

thermite is written in Rust and built with Cargo.

```sh
git clone https://github.com/your-org/thermite.git
cd thermite
cargo build --release
# Optionally install to ~/.cargo/bin
cargo install --path .
```

---

## Usage

```
thermite [OPTIONS] <COMMAND>

Options:
  -v, --verbose    Print each external command before it is executed
  -h, --help       Print help
  -V, --version    Print version

Commands:
  update    Package a new upstream Rust toolchain release for Ubuntu
  help      Print this message or the help of the given subcommand(s)
```

### `thermite update`

Creates a new versioned `rustc-X.Y` Ubuntu source package for a new upstream Rust release.

```
thermite update [OPTIONS]
  -u, --rust-update-version <X.Y.Z>   Rust version being packaged  (e.g. 1.85.1)
  -o, --rust-old-version    <X.Y.Z>   Rust version being replaced  (e.g. 1.84.0)
  -r, --release             <NAME>    Target Ubuntu release         (e.g. noble)
  -l, --lpuser              <NAME>    Launchpad username
  -b, --lp-bug-number       <NUMBER>  Launchpad bug ID for this work
  -g, --git-remote          <NAME>    Local remote for Foundations rustc repo
                                      [default: foundations]
  -d, --repo-dir            <PATH>    Debian source package root
                                      [default: current directory]
```

#### Example: update Rust from 1.84.0 to 1.85.1 for Noble

```sh
cd ~/rustc/rustc          # the cloned Foundations rustc Git repository
thermite update \
  --rust-update-version 1.85.1 \
  --rust-old-version    1.84.0 \
  --release             noble \
  --lpuser              jdoe \
  --lp-bug-number       2109761
```

#### Example: same update with verbose command output

```sh
thermite --verbose update \
  --rust-update-version 1.85.1 \
  --rust-old-version    1.84.0 \
  --release             noble \
  --lpuser              jdoe \
  --lp-bug-number       2109761
```

The `--verbose` flag prints every external command (prefixed with `+`) before it is
executed, which is useful for understanding exactly what thermite is doing or for
diagnosing failures.

---

## Workflow Phases (update command)

thermite runs the following phases in sequence.  Fully automated phases execute without
user input.  Interactive phases print instructions and wait for a keypress before
continuing.

| Phase | Description | Mode |
|-------|-------------|------|
| 0 | Preflight checks — verify required tools and repository layout | Automated |
| 1 | Create a Launchpad bug report | Interactive |
| 2 | Set up Git branch (`merge-X.Y`) | Automated |
| 3 | Update `debian/changelog`, package name, and `debian/watch` | Automated |
| 4 | Temporarily include all vendored dependencies in `debian/copyright` | Automated |
| 5 | Download upstream source with `uscan` (first pass, full vendor) | Automated |
| 6 | Import upstream source into Git with `gbp import-orig` (first pass) | Automated |
| 7 | Refresh patches with `quilt` | Interactive |
| 8 | Prune unwanted vendored dependencies with `cargo-vendor-filterer` | Automated |
| 9 | Remove vendored C libraries | Interactive |
| 10 | Download upstream source again (second pass, pruned vendor) | Automated |
| 11 | Update versioned package references in control files | Automated |
| 12 | After-repack patch refreshes | Interactive |
| 13 | Update `XS-Vendored-Sources-Rust` in `debian/control` | Automated |
| 14 | Update vendored copyright overrides | Automated |
| 15 | Update `debian/copyright` stanzas | Interactive |
| 16 | Local build and bug fixing with `sbuild` | Interactive |
| 17 | Lintian checks | Interactive |
| 18 | PPA build — upload to Launchpad PPA and verify | Interactive |
| 19 | Run `autopkgtest`s | Interactive |
| 20 | Upload and request sponsorship | Interactive |

---

## Project Structure

```
src/
  bin/thermite.rs       CLI entry point (clap argument parsing and dispatch)
  commands/
    update.rs           Orchestrates the full update workflow
  steps/
    autopkgtest.rs      autopkgtest invocations
    build.rs            dpkg-buildpackage and sbuild
    changelog.rs        dch and debian/changelog editing
    control.rs          debian/control and debian/control.in editing
    copyright.rs        debian/copyright editing
    gbp.rs              gbp import-orig
    git.rs              Git operations
    lintian.rs          lintian invocations
    patches.rs          quilt push/refresh
    ppa.rs              ppa-dev-tools and dput
    uscan.rs            uscan and orig tarball management
    vendor.rs           cargo-vendor-filterer and vendor-tarball rule
  types/
    params.rs           UpdateParams — validated CLI parameters
    ubuntu.rs           UbuntuRelease — validated Ubuntu release names
    versions.rs         RustVersion — X.Y.Z and X.Y version newtypes
  error.rs              Unified ThermiteError type (thiserror)
  shell.rs              Async external command runner with streaming output
  ui.rs                 Terminal output helpers (phase headers, prompts, countdown)
  lib.rs                Crate root
```

---

## AI Disclosure

> **Important:** This project was planned, generated, and iteratively refined with the
> assistance of Large Language Model (LLM) tools, specifically GitHub Copilot (powered by
> Claude Sonnet).

LLM assistance was used throughout the development of thermite, including:

- **Planning** — the implementation plans in `/plans/` were drafted with LLM assistance
  based on the upstream Ubuntu Rust packaging documentation.
- **Code generation** — the majority of the Rust source code was generated or substantially
  revised through LLM-assisted sessions in VS Code.
- **Bug fixing** — several correctness issues (error handling, version substitution in
  `debian/watch`, rate-limit retry logic, etc.) were identified and resolved through
  LLM-assisted review.
- **Documentation** — this README and inline doc comments were written with LLM assistance.

All generated code and documentation has been reviewed by human maintainers, but users
should be aware that LLM-generated code can contain subtle errors.  Before running
thermite against a real Ubuntu package repository, review the workflow phases carefully
and verify that the tool's actions match the upstream documentation linked above.

If you find a bug or an incorrect workflow step, please open an issue.

---

## License

See [LICENSE](LICENSE).
