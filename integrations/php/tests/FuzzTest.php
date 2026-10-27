<?php

declare(strict_types=1);

namespace Trongoscript\Tests;

use Random\Engine\Mt19937;
use Random\Randomizer;
use Trongoscript\Context;
use Trongoscript\Exception;

use function Trongoscript\dump;
use function Trongoscript\evaluate;

/**
 * Randomized checks. Each test seeds its own generator, so a failure is
 * reproducible from the seed in its message (override with FUZZ_SEED).
 */
final class FuzzTest extends TestCase
{
    private const ALPHABET = ['a', 'Z', '0', ' ', '"', '\\', '#', '{', '}', '$', "\n", "\t", "\0", 'é', '☃', '𝄞', '(', ')', ';', ':'];

    private static function seed(): int
    {
        return (int) (getenv('FUZZ_SEED') ?: 20261020);
    }

    private static function randomString(Randomizer $r): string
    {
        $s = '';
        for ($i = $r->getInt(0, 7); $i > 0; $i--) {
            $s .= self::ALPHABET[$r->getInt(0, count(self::ALPHABET) - 1)];
        }
        return $s;
    }

    /**
     * A random value whose PHP shape survives a round trip: map keys are
     * never PHP-numeric strings (those turn into int keys and lists), and
     * maps are never empty (`[]` comes back as a list).
     */
    private static function randomValue(Randomizer $r, int $depth = 0): mixed
    {
        switch ($r->getInt(0, $depth > 4 ? 5 : 7)) {
            case 0: return null;
            case 1: return $r->getInt(0, 1) === 1;
            case 2: return $r->getInt(PHP_INT_MIN, PHP_INT_MAX);
            case 3: return ($r->getFloat(0, 1) - 0.5) * 10 ** $r->getInt(-20, 20);
            case 4: return self::randomString($r);
            case 5: return $r->getInt(-1000, 1000);
            case 6:
                $list = [];
                for ($i = $r->getInt(0, 4); $i > 0; $i--) {
                    $list[] = self::randomValue($r, $depth + 1);
                }
                return $list;
            default:
                $map = ['k' . self::randomString($r) => self::randomValue($r, $depth + 1)];
                for ($i = $r->getInt(0, 3); $i > 0; $i--) {
                    $map['k' . self::randomString($r)] = self::randomValue($r, $depth + 1);
                }
                ksort($map, SORT_STRING);
                return $map;
        }
    }

    /**
     * dump() writes whole-valued floats without a decimal point, so they
     * come back as ints: by design below 2**53, where the value is the
     * same; as a different number at or past it (the skipped test below).
     */
    private static function dumpLossy(mixed $v): bool
    {
        if (is_float($v)) {
            return floor($v) === $v;
        }
        return is_array($v) && array_filter($v, [self::class, 'dumpLossy']) !== [];
    }

    public function testValuesRoundTripThroughGlobalsAndEval(): void
    {
        $r = new Randomizer(new Mt19937(self::seed()));
        $ctx = new Context();
        for ($i = 0; $i < 300; $i++) {
            $v = self::randomValue($r);
            $ctx->set('v', $v);
            $this->assertSame($v, $ctx->eval('v'), 'seed ' . self::seed() . ", case $i");
        }
    }

    public function testValuesRoundTripThroughDump(): void
    {
        $r = new Randomizer(new Mt19937(self::seed() + 1));
        for ($i = 0; $i < 300; $i++) {
            $v = self::randomValue($r);
            if (self::dumpLossy($v)) {
                continue;
            }
            $src = dump($v, $i % 2 === 1);
            $this->assertSame($v, evaluate($src), 'seed ' . (self::seed() + 1) . ", case $i: $src");
        }
    }

    public function testValuesRoundTripThroughHostFunctions(): void
    {
        $r = new Randomizer(new Mt19937(self::seed() + 2));
        $ctx = new Context(['functions' => ['echo' => fn ($x) => $x]]);
        for ($i = 0; $i < 100; $i++) {
            $v = self::randomValue($r);
            $ctx->set('v', $v);
            $this->assertSame($v, $ctx->eval('(echo v)'), 'seed ' . (self::seed() + 2) . ", case $i");
        }
    }

    private const TOKENS = ['(', ')', '[', ']', '{', '}', '1', '-2.5', '"s"', ':k', 'x', '+', 'fn', 'let', 'def', 'if',
        'nil', 'true', "'", '`', '~', '~@', '#{', 'map', 'reduce', 'range', 'str', "\n", ' ', ';', '&',
        'defmacro', 'include', 'read-file', 'quote', '1e999', '99999999999999999999', '\\', '"'];

    public function testRandomSourceNeverEscapesTheErrorHierarchy(): void
    {
        $r = new Randomizer(new Mt19937(self::seed() + 3));
        $ctx = new Context(['limits' => ['max_steps' => 20_000, 'max_value_len' => 10_000]]);
        for ($i = 0; $i < 1500; $i++) {
            $src = '';
            for ($n = $r->getInt(1, 30); $n > 0; $n--) {
                $src .= self::TOKENS[$r->getInt(0, count(self::TOKENS) - 1)] . ' ';
            }
            try {
                $ctx->eval($src);
            } catch (Exception) {
                // Any classified failure is fine; anything else is a bug.
            } catch (\Throwable $e) {
                $this->fail('seed ' . (self::seed() + 3) . ", case $i: " . json_encode($src) . ' threw ' . $e::class . ': ' . $e->getMessage());
            }
        }
        $this->addToAssertionCount(1);
    }

    public function testRandomBytesAsSource(): void
    {
        $r = new Randomizer(new Mt19937(self::seed() + 4));
        for ($i = 0; $i < 300; $i++) {
            try {
                evaluate($r->getBytes($r->getInt(1, 64)));
            } catch (Exception | \ValueError) {
                // Invalid UTF-8 is a ValueError before the parser sees it.
            }
        }
        $this->addToAssertionCount(1);
    }

    public function testDumpPreservesLargeNumbers(): void
    {
        $this->markTestSkipped('known core limitation: dump/eval is lossy for whole floats >= 2**53');
        // @phpstan-ignore deadCode.unreachable
        $f = -7.018252704029115e18;
        $this->assertEquals($f, evaluate(dump($f)));
    }
}
