{ pkgs, lib, config, inputs, ... }:

let
  php = config.languages.php.package;
in
{
  languages.php.enable = true;
  languages.rust.enable = true;

  # ext-php-rs reads the build configuration from `php-config`, which is
  # only in the unwrapped interpreter's dev output, and generates its
  # bindings with bindgen, which loads libclang.
  packages = [
    pkgs.git
    pkgs.openssl
    pkgs.valgrind
    php.unwrapped.dev
    php.packages.composer
  ];
  env.LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";

  # Run the test suite under valgrind's memcheck, with PHP's own allocator
  # off so every allocation is visible. Slow; pass PHPUnit arguments to
  # narrow it, e.g. `memcheck --filter ConversionTest`.
  #
  # The JITs are off because memcheck cannot see into generated code:
  # PCRE's reads past string ends with SIMD loads, which memcheck reports.
  #
  # `php` here is a wrapper that execs the real interpreter, hence
  # --trace-children. It also puts the child processes FatalTest starts
  # under memcheck, and a memcheck error there fails that test.
  scripts.memcheck.exec = ''
    set -euo pipefail
    cargo build
    USE_ZEND_ALLOC=0 ZEND_DONT_UNLOAD_MODULES=1 exec valgrind \
      --trace-children=yes --error-exitcode=99 --leak-check=no --track-origins=yes \
      php -d pcre.jit=0 -d opcache.enable_cli=0 \
      -d extension=target/debug/libtrongoscript.so vendor/bin/phpunit "$@"
  '';

  enterTest = ''
    composer install --no-interaction
    composer test
  '';
}
