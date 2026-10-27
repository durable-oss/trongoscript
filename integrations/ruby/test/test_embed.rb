# frozen_string_literal: true

require "test_helper"

class TestEmbed < Minitest::Test
  def test_string_and_map_markers
    doc = {
      "port" => "!trongo (+ 8000 80)",
      "list" => ['!trongo (upcase "a")', "plain", 1, nil],
      "map" => { "!trongo" => "{:x 1}" },
      "untouched" => "!trongoish",
      "not_single" => { "!trongo" => "(+ 1 1)", "other" => 1 }
    }
    assert_equal({
                   "port" => 8080,
                   "list" => ["A", "plain", 1, nil],
                   "map" => { "x" => 1 },
                   "untouched" => "!trongoish",
                   "not_single" => { "!trongo" => "(+ 1 1)", "other" => 1 }
                 }, Trongoscript.embed(doc))
  end

  def test_scalar_roots
    assert_equal 3, Trongoscript.embed("!trongo (+ 1 2)")
    assert_equal "plain", Trongoscript.embed("plain")
    assert_nil Trongoscript.embed(nil)
    assert_equal [3], Trongoscript.embed(["!trongo (+ 1 2)"])
  end

  def test_symbol_keys_in_the_document
    assert_equal({ "a" => 2 }, Trongoscript.embed({ a: "!trongo (+ 1 1)" }))
    assert_equal({ a: 2 }, Trongoscript.embed({ a: "!trongo (+ 1 1)" }, symbolize_names: true))
  end

  def test_custom_markers
    doc = { "a" => "=(+ 1 1)", "b" => { "$eval" => "(+ 2 2)" }, "c" => "!trongo (+ 1 1)" }
    assert_equal({ "a" => 2, "b" => 4, "c" => "!trongo (+ 1 1)" }, Trongoscript.embed(doc, prefix: "=", key: "$eval"))
  end

  def test_snippets_share_one_context
    ctx = Trongoscript::Context.new(globals: { "base" => 10 })
    out = ctx.embed({ "a" => "!trongo (def x (+ base 1))", "b" => "!trongo (* x 2)" })
    assert_equal({ "a" => 11, "b" => 22 }, out)
    assert_equal 11, ctx["x"]
  end

  def test_host_functions_in_snippets
    out = Trongoscript.embed({ "home" => '!trongo (env "HOME")' }, functions: { env: ->(k) { "/home/#{k.downcase}" } })
    assert_equal({ "home" => "/home/home" }, out)
  end

  def test_yaml_tags
    yaml = <<~YAML
      port: !trongo (+ 8000 80)
      name: "!trongo (upcase \\"x\\")"
      nested:
        - !trongo '{:a 1}'
        - keep
      anchor: &a [1, 2]
      alias: *a
      other: !custom kept
    YAML
    assert_equal({
                   "port" => 8080, "name" => "X", "nested" => [{ "a" => 1 }, "keep"],
                   "anchor" => [1, 2], "alias" => [1, 2], "other" => "kept"
                 }, Trongoscript.embed_yaml(yaml))
  end

  def test_yaml_custom_tag
    out = Trongoscript.embed_yaml("a: !calc (+ 1 2)\nb: !trongo (+ 1 1)\n", tag: "!calc")
    assert_equal({ "a" => 3, "b" => "(+ 1 1)" }, out)
  end

  def test_yaml_scalars_and_empty_documents
    assert_nil Trongoscript.embed_yaml("")
    assert_equal 3, Trongoscript.embed_yaml("!trongo (+ 1 2)")
    assert_equal [1, "two", 3.5, true, nil], Trongoscript.embed_yaml("[1, two, 3.5, true, null]")
  end

  def test_snippet_errors
    assert_raises(Trongoscript::EvalError) { Trongoscript.embed({ "a" => "!trongo (nope)" }) }
    assert_raises(Trongoscript::EvalError) { Trongoscript.embed_yaml("a: !trongo (nope)") }
    assert_raises(Trongoscript::EvalError) { Trongoscript.embed_yaml("a: !trongo [1, 2]") }
  end

  def test_document_conversion_errors
    assert_raises(TypeError) { Trongoscript.embed({ "a" => Object.new }) }
    assert_raises(TypeError) { Trongoscript.embed_yaml(42) }
  end
end
