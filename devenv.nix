{ pkgs, lib, config, inputs, ... }:

{
  # https://devenv.sh/basics/
  env.GREET = "devenv";

  # https://devenv.sh/packages/
  packages = [
    pkgs.git

    # Kani's `goto-cc` shells out to `gcc` to preprocess the generated C
    # before handing it to CBMC. Kani bundles CBMC, goto-cc, goto-instrument
    # and the kissat solver itself (under ~/.kani), so this compiler is the
    # only piece it expects to find on PATH.
    pkgs.gcc
  ];

  # https://devenv.sh/languages/
  #
  # Deliberately left on the default stable channel. Kani pins its own exact
  # nightly (0.67.0 wants nightly-2025-11-21) and switches to it internally,
  # so pinning nightly here would change what `cargo build`/`cargo test` run
  # on without affecting verification at all.
  languages.rust.enable = true;

  # https://devenv.sh/processes/
  # processes.dev.exec = "${lib.getExe pkgs.watchexec} -n -- ls -la";

  # https://devenv.sh/services/
  # services.postgres.enable = true;

  # https://devenv.sh/scripts/
  scripts.hello.exec = ''
    echo hello from $GREET
  '';

  # Install the Kani verifier and its CBMC bundle. `cargo kani setup` is
  # idempotent, but it downloads ~1GB on a cold cache, so this is a manual
  # step rather than part of enterShell.
  #
  # kani-verifier is a cargo plugin from crates.io, not a rustup component —
  # it cannot go in `languages.rust.components`.
  scripts.kani-setup.exec = ''
    set -euo pipefail
    if ! command -v cargo-kani >/dev/null 2>&1; then
      echo "installing kani-verifier..."
      cargo install --locked kani-verifier
    fi
    cargo kani setup
    echo "kani ready: $(cargo kani --version)"
  '';

  # Run the proof harnesses. Pass through any cargo-kani flags, e.g.
  #   verify -p trongoscript-stdlib
  #   verify --harness gcd_divides_both_inputs
  scripts.verify.exec = ''
    exec cargo kani "$@"
  '';

  # https://devenv.sh/basics/
  enterShell = ''
    hello         # Run scripts directly
    git --version # Use packages
    if command -v cargo-kani >/dev/null 2>&1; then
      echo "kani: $(cargo kani --version 2>/dev/null || echo 'run kani-setup')"
    else
      echo "kani: not installed — run 'kani-setup'"
    fi
  '';

  # https://devenv.sh/tasks/
  # tasks = {
  #   "myproj:setup".exec = "mytool build";
  #   "devenv:enterShell".after = [ "myproj:setup" ];
  # };

  # https://devenv.sh/tests/
  enterTest = ''
    echo "Running tests"
    git --version | grep --color=auto "${pkgs.git.version}"
    cargo test --workspace
  '';

  # https://devenv.sh/git-hooks/
  # git-hooks.hooks.shellcheck.enable = true;

  # See full reference at https://devenv.sh/reference/options/
}
