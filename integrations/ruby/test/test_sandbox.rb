# frozen_string_literal: true

require "test_helper"

class TestSandbox < Minitest::Test
  def test_directory_root
    with_tree("main.trongo" => '{:db (include "lib/db.trongo")}',
              "lib/db.trongo" => '{:host "localhost"}',
              "notes.txt" => "hello\n") do |dir|
      ctx = Trongoscript::Context.new(root: dir)
      assert_equal({ "db" => { "host" => "localhost" } }, ctx.eval_file("main.trongo"))
      assert_equal "hello\n", ctx.eval('(read-file "notes.txt")')
      assert_equal "hello\n", ctx.eval('(read-file "/notes.txt")')
      listing = ctx.eval('(list-files "lib")')
      assert_equal [{ "dir?" => false, "name" => "db.trongo" }], listing
    end
  end

  def test_pathname_root_and_mounts
    with_tree("root/a.txt" => "root", "vendor/b.txt" => "vendor") do |dir|
      ctx = Trongoscript::Context.new(root: Pathname(dir) / "root", mounts: { "/vendor" => Pathname(dir) / "vendor" })
      assert_equal %w[root vendor], ctx.eval('[(read-file "a.txt") (read-file "/vendor/b.txt")]')
    end
  end

  def test_symbol_mount_points
    with_tree("root/a.txt" => "a", "v/b.txt" => "b") do |dir|
      ctx = Trongoscript::Context.new(root: File.join(dir, "root"), mounts: { "/v": File.join(dir, "v") })
      assert_equal "b", ctx.eval('(read-file "/v/b.txt")')
    end
  end

  def test_escapes_are_refused
    with_tree("root/a.txt" => "a", "secret.txt" => "secret") do |dir|
      ctx = Trongoscript::Context.new(root: File.join(dir, "root"))
      ["../secret.txt", "/../secret.txt", "/etc/passwd", "#{dir}/secret.txt"].each do |path|
        assert_raises(Trongoscript::SandboxError, path) { ctx.eval("(read-file #{path.inspect})") }
      end
    end
  end

  def test_symlinks_out_of_the_sandbox_are_refused
    with_tree("root/a.txt" => "a", "secret.txt" => "secret") do |dir|
      File.symlink(File.join(dir, "secret.txt"), File.join(dir, "root", "link.txt"))
      ctx = Trongoscript::Context.new(root: File.join(dir, "root"))
      assert_raises(Trongoscript::SandboxError) { ctx.eval('(read-file "link.txt")') }
    end
  end

  def test_in_memory_files
    ctx = Trongoscript::Context.new(files: {
                                      "main.trongo" => '(include "lib/x.trongo")',
                                      "/lib/x.trongo" => "{:x 1}",
                                      Pathname("data/notes.txt") => "☃"
                                    })
    assert_equal({ "x" => 1 }, ctx.eval('(include "main.trongo")'))
    assert_equal "☃", ctx.eval('(read-file "data/notes.txt")')
    assert_equal [{ "dir?" => false, "name" => "x.trongo" }], ctx.eval('(list-files "lib")')
    assert_raises(Trongoscript::SandboxError) { ctx.eval('(read-file "missing")') }
    assert_raises(Trongoscript::SandboxError) { ctx.eval('(read-file "../main.trongo")') }
  end

  def test_include_cycles
    ctx = Trongoscript::Context.new(files: { "a.trongo" => '(include "b.trongo")', "b.trongo" => '(include "a.trongo")' })
    e = assert_raises(Trongoscript::SandboxError) { ctx.eval_file("a.trongo") }
    assert_includes e.message, "cycle"
  end

  def test_sandbox_limits
    ctx = Trongoscript::Context.new(files: { "big.txt" => "x" * 100 }, sandbox_limits: { max_file_bytes: 10 })
    e = assert_raises(Trongoscript::SandboxError) { ctx.eval('(read-file "big.txt")') }
    assert_includes e.message, "too large"

    chain = (0..5).to_h { |i| ["f#{i}.trongo", i == 5 ? "1" : "(include \"f#{i + 1}.trongo\")"] }
    ctx = Trongoscript::Context.new(files: chain, sandbox_limits: { "max_include_depth" => 3 })
    assert_raises(Trongoscript::SandboxError) { ctx.eval_file("f0.trongo") }
    ctx = Trongoscript::Context.new(files: chain)
    assert_equal 1, ctx.eval_file("f0.trongo")
  end

  def test_include_shares_definitions_with_the_context
    ctx = Trongoscript::Context.new(files: { "lib.trongo" => "(defn triple [x] (* 3 x))" })
    ctx.eval_file("lib.trongo")
    assert_equal 9, ctx.eval("(triple 3)")
  end
end
