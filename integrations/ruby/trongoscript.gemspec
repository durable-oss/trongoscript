# frozen_string_literal: true

require_relative "lib/trongoscript/version"

Gem::Specification.new do |spec|
  spec.name = "trongoscript"
  spec.version = Trongoscript::VERSION
  spec.authors = ["Durable Programming LLC"]
  spec.email = ["commercial@durableprogramming.com"]

  spec.summary = "Ruby bindings for Trongoscript, a small Lisp for building configuration data"
  spec.description = <<~DESC
    Evaluate Trongoscript documents, render ERB-style templates, and expand
    !trongo snippets embedded in YAML or JSON, from Ruby. Evaluation is
    sandboxed and metered; Ruby blocks can be exposed to scripts as functions.
  DESC
  spec.homepage = "https://github.com/durable-oss/trongoscript"
  spec.licenses = ["MIT", "Apache-2.0"]
  spec.required_ruby_version = ">= 3.1.0"

  spec.metadata = {
    "homepage_uri" => spec.homepage,
    "source_code_uri" => "#{spec.homepage}/tree/main/integrations/ruby",
    "changelog_uri" => "#{spec.homepage}/blob/main/integrations/ruby/CHANGELOG.md",
    "bug_tracker_uri" => "#{spec.homepage}/issues",
    "rubygems_mfa_required" => "true"
  }

  spec.files = Dir[
    "lib/**/*.rb",
    "ext/trongoscript/{Cargo.toml,Cargo.lock,extconf.rb}",
    "ext/trongoscript/src/**/*.rs",
    "README.md",
    "CHANGELOG.md",
    "LICENSE-APACHE",
    "LICENSE-MIT"
  ]
  spec.require_paths = ["lib"]
  spec.extensions = ["ext/trongoscript/extconf.rb"]

  spec.add_dependency "rb_sys", "~> 0.9"
end
