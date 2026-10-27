<?php

declare(strict_types=1);

namespace Trongoscript\Tests;

use Trongoscript\Context;
use Trongoscript\EvalException;

use function Trongoscript\embed;
use function Trongoscript\embed_yaml;

final class EmbedTest extends TestCase
{
    public function testStringAndMapMarkers(): void
    {
        $doc = [
            'port' => '!trongo (+ 8000 80)',
            'list' => ['!trongo (upcase "a")', 'plain', 1, null],
            'map' => ['!trongo' => '{:x 1}'],
            'untouched' => '!trongoish',
            'not_single' => ['!trongo' => '(+ 1 1)', 'other' => 1],
        ];
        $this->assertSame([
            'list' => ['A', 'plain', 1, null],
            'map' => ['x' => 1],
            'not_single' => ['!trongo' => '(+ 1 1)', 'other' => 1],
            'port' => 8080,
            'untouched' => '!trongoish',
        ], embed($doc));
    }

    public function testScalarRoots(): void
    {
        $this->assertSame(3, embed('!trongo (+ 1 2)'));
        $this->assertSame('plain', embed('plain'));
        $this->assertNull(embed(null));
        $this->assertSame([3], embed(['!trongo (+ 1 2)']));
    }

    public function testCustomMarkers(): void
    {
        $doc = ['a' => '=(+ 1 1)', 'b' => ['$eval' => '(+ 2 2)'], 'c' => '!trongo (+ 1 1)'];
        $expected = ['a' => 2, 'b' => 4, 'c' => '!trongo (+ 1 1)'];
        $this->assertSame($expected, embed($doc, ['prefix' => '=', 'key' => '$eval']));
        $this->assertSame($expected, (new Context())->embed($doc, ['prefix' => '=', 'key' => '$eval']));
    }

    public function testUnknownMarkers(): void
    {
        $this->expectException(\ValueError::class);
        (new Context())->embed([], ['root' => '/']);
    }

    public function testSnippetsShareOneContext(): void
    {
        $ctx = new Context(['globals' => ['base' => 10]]);
        $out = $ctx->embed(['a' => '!trongo (def x (+ base 1))', 'b' => '!trongo (* x 2)']);
        $this->assertSame(['a' => 11, 'b' => 22], $out);
        $this->assertSame(11, $ctx->get('x'));
    }

    public function testHostFunctionsInSnippets(): void
    {
        $out = embed(['home' => '!trongo (env "HOME")'], ['functions' => ['env' => fn ($k) => '/home/' . strtolower($k)]]);
        $this->assertSame(['home' => '/home/home'], $out);
    }

    public function testObjectsMode(): void
    {
        $out = embed((object) ['a' => '!trongo {}', 'b' => []], ['objects' => true]);
        $this->assertSame('{"a":{},"b":[]}', json_encode($out));
    }

    public function testYamlTags(): void
    {
        $yaml = <<<'YAML'
            port: !trongo (+ 8000 80)
            name: "!trongo (upcase \"x\")"
            nested:
              - !trongo '{:a 1}'
              - keep
            anchor: &a [1, 2]
            alias: *a
            other: !custom kept
            YAML;
        $this->assertSame([
            'alias' => [1, 2], 'anchor' => [1, 2], 'name' => 'X',
            'nested' => [['a' => 1], 'keep'], 'other' => 'kept', 'port' => 8080,
        ], embed_yaml($yaml));
    }

    public function testYamlCustomTag(): void
    {
        $this->assertSame(['a' => 3, 'b' => '(+ 1 1)'], embed_yaml("a: !calc (+ 1 2)\nb: !trongo (+ 1 1)\n", ['tag' => '!calc']));
        $this->assertSame(['a' => 3], (new Context())->embedYaml('a: !calc (+ 1 2)', ['tag' => '!calc']));
    }

    public function testYamlScalarsAndEmptyDocuments(): void
    {
        $this->assertNull(embed_yaml(''));
        $this->assertSame(3, embed_yaml('!trongo (+ 1 2)'));
        $this->assertSame([1, 'two', 3.5, true, null], embed_yaml('[1, two, 3.5, true, null]'));
    }

    public function testSnippetErrors(): void
    {
        foreach ([fn () => embed(['a' => '!trongo (nope)']), fn () => embed_yaml('a: !trongo (nope)'), fn () => embed_yaml('a: !trongo [1, 2]')] as $fn) {
            try {
                $fn();
                $this->fail('no exception');
            } catch (EvalException) {
                $this->addToAssertionCount(1);
            }
        }
    }

    public function testDocumentConversionErrors(): void
    {
        $this->expectException(\TypeError::class);
        embed(['a' => new \DateTime()]);
    }
}
