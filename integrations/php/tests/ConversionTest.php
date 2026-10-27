<?php

declare(strict_types=1);

namespace Trongoscript\Tests;

use PHPUnit\Framework\Attributes\DataProvider;
use Trongoscript\Context;
use Trongoscript\ResourceLimitException;

/** Values crossing between PHP and Trongoscript, in both directions. */
final class ConversionTest extends TestCase
{
    private Context $ctx;

    protected function setUp(): void
    {
        $this->ctx = new Context();
    }

    private function roundTrip(mixed $value): mixed
    {
        $this->ctx->set('v', $value);
        $viaEval = $this->ctx->eval('v');
        $this->assertSame($viaEval, $this->ctx->get('v'));
        return $viaEval;
    }

    public static function supportedValues(): iterable
    {
        yield 'null' => [null];
        yield 'true' => [true];
        yield 'false' => [false];
        yield 'zero' => [0];
        yield 'int' => [-12345];
        yield 'int max' => [PHP_INT_MAX];
        yield 'int min' => [PHP_INT_MIN];
        yield 'float' => [1.5];
        yield 'negative float' => [-2.25];
        yield 'huge float' => [1e300];
        yield 'tiny float' => [-1e-300];
        yield 'float max' => [PHP_FLOAT_MAX];
        yield 'empty string' => [''];
        yield 'nul byte' => ["a\0b"];
        yield 'unicode' => ['héllo ☃ 𝄞'];
        yield 'long string' => [str_repeat('x', 100_000)];
        yield 'empty array' => [[]];
        yield 'list' => [[1, [2, [3]]]];
        yield 'map' => [['a' => ['b' => [null, ['c' => 'd']]]]];
        yield 'big list' => [range(1, 1000)];
    }

    #[DataProvider('supportedValues')]
    public function testRoundTrips(mixed $value): void
    {
        $this->assertSame($value, $this->roundTrip($value));
    }

    public function testWholeFloatsStayFloats(): void
    {
        $this->assertSame(2.0, $this->roundTrip(2.0));
    }

    public function testNegativeZero(): void
    {
        $got = $this->roundTrip(-0.0);
        $this->assertIsFloat($got);
        $this->assertEquals(0.0, $got);
    }

    public function testListDetection(): void
    {
        $this->assertSame([1, 2], $this->roundTrip([0 => 1, 1 => 2]));
        // Keys other than 0, 1, 2, ... in order make a map, as with
        // json_encode. Maps come back sorted by key.
        $this->assertSame([0 => 'a', 1 => 'b'], $this->roundTrip([1 => 'b', 0 => 'a']));
        $this->assertSame([1 => 'a', 2 => 'b'], $this->roundTrip([1 => 'a', 2 => 'b']));
        $this->assertSame('{"5":"x"}', json_encode($this->roundTrip([5 => 'x'])));
    }

    public function testSparseAndReorderedArraysAreMaps(): void
    {
        $this->ctx->set('v', [1 => 'b', 0 => 'a']);
        $this->assertSame('map', $this->ctx->eval('(if (map? v) "map" "list")'));
        $this->ctx->set('v', [0 => 'a', 1 => 'b']);
        $this->assertSame('list', $this->ctx->eval('(if (map? v) "map" "list")'));
    }

    public function testNumericKeysFollowPhp(): void
    {
        $got = $this->ctx->eval('{"0" :a "1" :b "-5" :c "01" :d "-0" :e "1.5" :f " 1" :g "9223372036854775808" :h}');
        $this->assertSame('b', $got[1]);
        $this->assertSame('b', $got['1'], 'a numeric string key is reachable both ways');
        $this->assertSame('c', $got[-5]);
        $this->assertSame('d', $got['01']);
        $this->assertSame('e', $got['-0']);
        $this->assertSame('f', $got['1.5']);
        $this->assertSame('g', $got[' 1']);
        $this->assertSame('h', $got['9223372036854775808']);
        foreach (array_keys($got) as $k) {
            if (in_array($k, [0, 1, -5], true)) {
                $this->assertIsInt($k);
            } else {
                $this->assertIsString($k);
            }
        }
    }

    public function testIntegerKeysBecomeStringsInScripts(): void
    {
        $this->ctx->set('v', [10 => 'ten', 'x' => 'y']);
        $this->assertSame(['10', 'x'], $this->ctx->eval('(keys v)'));
    }

    public function testStdClassIn(): void
    {
        $o = new \stdClass();
        $o->a = 1;
        $o->b = new \stdClass();
        $o->b->c = [new \stdClass()];
        $this->assertSame(['a' => 1, 'b' => ['c' => [[]]]], $this->roundTrip($o));
        $this->ctx->set('o', $o);
        $this->assertSame(['a', 'b'], $this->ctx->eval('(keys o)'));
    }

    public function testEmptyStdClassIsAnEmptyMap(): void
    {
        $this->ctx->set('o', new \stdClass());
        $this->assertTrue($this->ctx->eval('(map? o)'));
    }

    public function testObjectsModeKeepsMapsAndListsApart(): void
    {
        $ctx = new Context(['objects' => true]);
        $ctx->set('v', ['empty_map' => new \stdClass(), 'empty_list' => [], 'zero' => (object) ['0' => 'x']]);
        $v = $ctx->eval('v');
        $this->assertEquals(new \stdClass(), $v->empty_map);
        $this->assertSame([], $v->empty_list);
        $this->assertSame('x', $v->zero->{'0'});
        $this->assertSame('{"empty_list":[],"empty_map":{},"zero":{"0":"x"}}', json_encode($v));
    }

    public function testJsonSerializableResultsAreCheckedToo(): void
    {
        $obj = new class implements \JsonSerializable {
            public function jsonSerialize(): mixed
            {
                return ['kind' => 'custom', 'nested' => new \ArrayObject()];
            }
        };
        $this->expectException(\TypeError::class);
        $this->ctx->set('v', $obj);
    }

    public function testJsonSerializableReturningData(): void
    {
        $obj = new class implements \JsonSerializable {
            public function jsonSerialize(): mixed
            {
                return ['kind' => 'custom', 'n' => [1, 2]];
            }
        };
        $this->assertSame(['kind' => 'custom', 'n' => [1, 2]], $this->roundTrip($obj));
        $this->assertSame([['kind' => 'custom', 'n' => [1, 2]]], $this->roundTrip([$obj]));
    }

    public function testJsonSerializableReturningItselfEnds(): void
    {
        $obj = new class implements \JsonSerializable {
            public function jsonSerialize(): mixed
            {
                return $this;
            }
        };
        $this->expectException(\ValueError::class);
        $this->ctx->set('v', $obj);
    }

    public function testJsonSerializableThatThrows(): void
    {
        $obj = new class implements \JsonSerializable {
            public function jsonSerialize(): mixed
            {
                throw new \DomainException('no');
            }
        };
        try {
            $this->ctx->set('v', $obj);
            $this->fail('expected an exception');
        } catch (\DomainException $e) {
            $this->assertSame('no', $e->getMessage());
        } catch (\TypeError) {
            // Also acceptable: the failure was reported as a conversion error.
            $this->addToAssertionCount(1);
        }
    }

    public static function unsupportedValues(): iterable
    {
        yield 'DateTime' => [new \DateTime()];
        yield 'Closure' => [fn () => 1];
        yield 'ArrayObject' => [new \ArrayObject([1])];
        yield 'nested object' => [['a' => [1, new \SplStack()]]];
    }

    #[DataProvider('unsupportedValues')]
    public function testUnsupportedValues(mixed $value): void
    {
        $this->expectException(\TypeError::class);
        $this->expectExceptionMessage('cannot convert');
        $this->ctx->set('v', $value);
    }

    public function testResources(): void
    {
        $r = fopen('php://memory', 'r');
        try {
            $this->expectException(\TypeError::class);
            $this->ctx->set('v', $r);
        } finally {
            fclose($r);
        }
    }

    public function testNonFiniteFloats(): void
    {
        foreach ([NAN, INF, -INF] as $f) {
            try {
                $this->ctx->set('v', $f);
                $this->fail("accepted $f");
            } catch (\ValueError $e) {
                $this->assertStringContainsString('no Trongoscript representation', $e->getMessage());
            }
        }
    }

    public function testInvalidUtf8(): void
    {
        foreach (["\xFF", ['k' => "\xFE"], ["\xFF" => 1], [1, ['a' => ["\xC0\x80" => 1]]], (object) ["\xFF" => 1]] as $v) {
            try {
                $this->ctx->set('v', $v);
                $this->fail('accepted invalid UTF-8');
            } catch (\ValueError $e) {
                $this->assertStringContainsString('UTF-8', $e->getMessage());
            }
        }
    }

    public function testReferencesAreFollowed(): void
    {
        $x = 1;
        $arr = ['a' => &$x, 'b' => [&$x]];
        $this->assertSame(['a' => 1, 'b' => [1]], $this->roundTrip($arr));
    }

    public function testRecursiveArraysAreRefused(): void
    {
        $a = [];
        $a[] = &$a;
        $this->expectException(\ValueError::class);
        $this->expectExceptionMessage('1024');
        $this->ctx->set('v', $a);
    }

    public function testRecursiveObjectsAreRefused(): void
    {
        $o = new \stdClass();
        $o->self = $o;
        $this->expectException(\ValueError::class);
        $this->ctx->set('v', $o);
    }

    public function testSharedButAcyclicObjects(): void
    {
        $shared = (object) ['x' => 1];
        $this->assertSame([['x' => 1], ['x' => 1]], $this->roundTrip([$shared, $shared]));
    }

    public function testNestingLimitOnTheWayIn(): void
    {
        $this->ctx->set('v', self::nested(1024));
        $this->assertSame(self::nested(1024), $this->ctx->get('v'));
        $this->expectException(\ValueError::class);
        $this->ctx->set('v', self::nested(1025));
    }

    public function testNestingLimitOnTheWayOut(): void
    {
        $this->assertSame(self::nested(1023, []), $this->ctx->eval(self::deepListSource(1024)));
        $this->expectException(ResourceLimitException::class);
        $this->ctx->eval(self::deepListSource(1025));
    }

    public function testScriptsMayKeepDeepValuesInternally(): void
    {
        $this->ctx->eval('(def deep ' . self::deepListSource(2000) . ') nil');
        $this->assertSame(1, $this->ctx->eval('(count deep)'));
        $this->assertTrue($this->ctx->has('deep'));
        try {
            $this->ctx->get('deep');
            $this->fail('handed over a value nested too deeply');
        } catch (ResourceLimitException) {
            $this->addToAssertionCount(1);
        }
        try {
            $this->ctx->globals();
            $this->fail('handed over a value nested too deeply');
        } catch (ResourceLimitException) {
            $this->addToAssertionCount(1);
        }
        $this->assertTrue($this->ctx->remove('deep'));
    }

    public function testUnsignedIntegersBecomeFloats(): void
    {
        // PHP has no unsigned integers; json_decode makes the same choice.
        $this->assertSame(18446744073709551615.0, $this->ctx->eval('(use serde) (parse-json "18446744073709551615")'));
        $this->assertSame(PHP_INT_MAX, $this->ctx->eval('(use serde) (parse-json "9223372036854775807")'));
    }

    public function testValidationDoesNotPartiallyApply(): void
    {
        $this->ctx->set('v', 1);
        try {
            $this->ctx->set('v', [1, new \DateTime()]);
            $this->fail('accepted a DateTime');
        } catch (\TypeError) {
            $this->assertSame(1, $this->ctx->get('v'));
        }
    }

    public function testLargeCollections(): void
    {
        $big = range(0, 199_999);
        $this->assertSame($big, $this->roundTrip($big));
        $wide = [];
        for ($i = 1; $i <= 50_000; $i++) {
            $wide["key$i"] = ['n' => $i];
        }
        ksort($wide, SORT_STRING);
        $this->assertSame($wide, $this->roundTrip($wide));
    }
}
