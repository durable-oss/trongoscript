{ pkgs, lib, config, inputs, ... }:

{
  packages = with pkgs; [ git libyaml openssl ];

  languages.ruby.enable = true;
  languages.rust.enable = true;

  # rb-sys generates its Ruby bindings with bindgen, which loads libclang
  # at build time.
  env.LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";

  enterTest = ''
    bundle install
    bundle exec rake
  '';
}
