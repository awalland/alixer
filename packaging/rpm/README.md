# Building and installing the RPM

The first tested target is openSUSE Tumbleweed x86_64. The spec also has Fedora
dependency/scriptlet conditionals, but a Fedora build needs separate validation.
Build on the oldest distribution you intend to support: a Tumbleweed build can
require newer glibc or native libraries than other RPM distributions provide.

Build tools: Rust and Cargo >= 1.95, a C compiler, Clang and its shared library,
PipeWire/SPA and SQLite development packages, pkg-config, systemd RPM macros,
desktop-file-utils, rpmbuild, zstd, jq and ripgrep. On Tumbleweed the native
development packages include pipewire-devel, sqlite3-devel and libclang13.

A `cargo rpm` subcommand wraps the scripts in this directory. Link it once:

```sh
ln -s "$PWD/packaging/rpm/cargo-rpm.sh" ~/.cargo/bin/cargo-rpm
```

Then `cargo rpm [build|check|publish] [args...]` runs the matching script
(`build` is the default).

Run from the checkout:

```sh
bash packaging/rpm/build.sh
```

The script vendors Cargo.lock dependencies, prepares source archives and license
notices, then builds both source and binary RPMs. Downloading dependencies occurs
before rpmbuild; compilation and tests run with Cargo's --frozen flag. Artifacts
and build caches are kept under target/rpm. Build concurrency defaults to two
jobs; CARGO_BUILD_JOBS overrides it.

Publish the binary RPMs of the current workspace version to the Gitea RPM registry,
and its source RPM as the generic package `alixer-source`:

```sh
bash packaging/rpm/build.sh --publish
# or, after a previous build:
bash packaging/rpm/publish.sh
```

Publishing reads `publish.env` in the repository root (mode 600, gitignored) or
the file named by `ALIXER_PUBLISH_CONFIG`. Start from the template that sits
next to it: `install -m 600 publish.env.example publish.env`, then edit it. It needs
`GITEA_BASE_URL`, `GITEA_USERNAME`, `GITEA_PACKAGE_OWNER` (the user or
organisation that owns the registry, `packages` in the examples) and `GITEA_TOKEN`.
Create the token in Gitea under Settings / Applications with only the
`write:package` scope. Only RPMs matching the workspace version are uploaded, so
stale artifacts under target/rpm are never published. The token is passed to
curl via stdin, never as a command-line argument.

Source RPMs are stored separately because their headers report the build
architecture, which would collide with the binary in Gitea's RPM registry.

If Rust is provided by rustup rather than RPM, RPM's build-dependency check may
not recognize it. After verifying all required tools/libraries are installed,
use `bash packaging/rpm/build.sh --skip-build-deps`. This does not make the
runtime dependencies optional. `--prepare-only` generates inputs without building.

The RPM requires the LSP LV2 effects package (`lv2-lsp-plugins` on openSUSE,
`lsp-plugins-lv2` on Fedora) for the dedicated EQ, compressor and limiter editors.

Install the matching binary RPM with zypper (replace the filename as needed):

```sh
sudo zypper install ./target/rpm/RPMS/x86_64/alixer-0.2.13-1.x86_64.rpm
alixerctl enable
systemctl --user daemon-reload
systemctl --user restart wireplumber.service
systemctl --user enable --now alixer.service
alixer
```

Restarting WirePlumber briefly interrupts audio. Installation itself does not
start the mixer or globally activate its WirePlumber hook. Each user opts in
with alixerctl enable. It creates only the user's activation fragment, using
the RPM-owned service and Lua script. Generated development copies are removed
when switching to the RPM; custom overrides are preserved and reported.
The fragment enables a small virtual component that requests the script as an
optional dependency. The script is loaded when installed, and WirePlumber can
still start if the package is later removed while that user fragment remains.

To stop using the integration:

```sh
systemctl --user disable --now alixer.service
alixerctl disable
systemctl --user restart wireplumber.service
```

Preferences, mixer state and custom output-name rules remain user-owned and are
not removed by uninstalling the RPM. The package includes dependency license
notices under its RPM license directory. This is an upstream vendored build;
submission to an official distribution repository may need further adaptation
to that distribution's Rust dependency and licensing policies.

The private audio integration test can also exercise extracted RPM binaries:
set `ALIXER_TEST_BIN_DIR` to their `usr/bin` directory and run
`python3 tests/runtime.py` (and `python3 tests/runtime.py --offline`). This loads
the adjacent packaged Lua script while leaving the desktop audio session alone.
`bash packaging/rpm/check.sh path/to/alixer.rpm` extracts and checks the package,
then runs both private integration tests. It needs the runtime audio tools,
Python, a C compiler for the test LV2 plugin, D-Bus, rpm2cpio and cpio; it never installs the RPM or restarts desktop
services. Extracted test files are retained under target/rpm for inspection.
