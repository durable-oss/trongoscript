<?php

declare(strict_types=1);

namespace Trongoscript\Tests;

use Trongoscript\ConcurrencyException;
use Trongoscript\Context;
use Trongoscript\EvalException;

use function Trongoscript\evaluate;

final class Doubler
{
    public function __invoke(int $x): int
    {
        return $x * 2;
    }

    public function triple(int $x): int
    {
        return $x * 3;
    }

    public static function quad(int $x): int
    {
        return $x * 4;
    }
}

final class CustomException extends \RuntimeException
{
}

/** PHP callables exposed to scripts. */
final class FunctionsTest extends TestCase
{
    private Context $ctx;

    protected function setUp(): void
    {
        $this->ctx = new Context();
    }

    public function testEveryKindOfCallable(): void
    {
        $d = new Doubler();
        $this->ctx
            ->define('closure', fn ($x) => $x + 1)
            ->define('named', 'strtoupper')
            ->define('invokable', $d)
            ->define('method', [$d, 'triple'])
            ->define('static', [Doubler::class, 'quad'])
            ->define('static-string', Doubler::class . '::quad')
            ->define('first-class', strrev(...));
        $this->assertSame(
            [11, 'ABC', 20, 30, 40, 40, 'cba'],
            $this->ctx->eval('[(closure 10) (named "abc") (invokable 10) (method 10) (static 10) (static-string 10) (first-class "abc")]'),
        );
    }

    public function testDefineReturnsTheContext(): void
    {
        $this->assertSame($this->ctx, $this->ctx->define('a', fn () => 1));
        $this->ctx->define('b', fn () => 2);
        $this->assertSame(['a', 'b'], $this->ctx->functions());
    }

    public function testFunctionsOption(): void
    {
        $ctx = new Context(['functions' => ['inc' => fn ($x) => $x + 1, 'dec' => fn ($x) => $x - 1]]);
        $this->assertSame([2, 0], $ctx->eval('[(inc 1) (dec 1)]'));
        $this->assertSame(['a' => 2], evaluate('{:a (inc 1)}', ['functions' => ['inc' => fn ($x) => $x + 1]]));
    }

    public function testArgumentsAreConverted(): void
    {
        $seen = null;
        $this->ctx->define('spy', function (...$args) use (&$seen) {
            $seen = $args;
        });
        $this->ctx->eval('(spy 1 2.5 "s" :kw nil true [1 [2]] {:a {:b 1}})');
        $this->assertSame([1, 2.5, 's', 'kw', null, true, [1, [2]], ['a' => ['b' => 1]]], $seen);
    }

    public function testArgumentsInObjectsMode(): void
    {
        $seen = null;
        $ctx = new Context(['objects' => true]);
        $ctx->define('spy', function ($m) use (&$seen) {
            $seen = $m;
        });
        $ctx->eval('(spy {:a {:b [{:c 1}]}})');
        $this->assertInstanceOf(\stdClass::class, $seen);
        $this->assertSame(1, $seen->a->b[0]->c);
    }

    public function testZeroAndManyArguments(): void
    {
        $this->ctx->define('count-args', fn (...$a) => count($a));
        $this->assertSame(0, $this->ctx->eval('(count-args)'));
        $this->assertSame(50, $this->ctx->eval('(count-args ' . implode(' ', range(1, 50)) . ')'));
    }

    public function testArityFollowsTheCallable(): void
    {
        $this->ctx->define('strict', fn (int $a, int $b) => $a + $b);
        $this->assertSame(3, $this->ctx->eval('(strict 1 2)'));
        $this->expectException(\ArgumentCountError::class);
        $this->ctx->eval('(strict 1)');
    }

    public function testPhpTypeErrorsFromTheCallable(): void
    {
        $this->ctx->define('ints', fn (int $a) => $a);
        $this->expectException(\TypeError::class);
        $this->ctx->eval('(ints "nope")');
    }

    public function testReturnValuesAreConverted(): void
    {
        $this->ctx->define('data', fn () => ['a' => [1, 'x', null, (object) ['x' => 1.5]]]);
        $this->assertSame(['a' => [1, 'x', null, ['x' => 1.5]]], $this->ctx->eval('(data)'));
        $this->ctx->define('nothing', function (): void {
        });
        $this->assertNull($this->ctx->eval('(nothing)'));
    }

    public function testUnconvertibleReturnValues(): void
    {
        $this->ctx->define('bad', fn () => new \DateTime());
        $this->expectException(\TypeError::class);
        $this->expectExceptionMessage('cannot convert DateTime');
        $this->ctx->eval('(bad)');
    }

    public function testReturnValuesNestedTooDeeply(): void
    {
        $this->ctx->define('deep', fn () => self::nested(2000));
        $this->expectException(\ValueError::class);
        $this->ctx->eval('(deep)');
    }

    public function testResultsFlowBackIntoTheScript(): void
    {
        $this->ctx->define('lookup', fn ($k) => ['port' => 8080][$k]);
        $this->assertSame(['url' => 'http://localhost:8080'], $this->ctx->eval('{:url (str "http://localhost:" (lookup "port"))}'));
    }

    public function testCallsHappenInOrderExactlyOnce(): void
    {
        $log = [];
        $this->ctx->define('log', function ($x) use (&$log) {
            $log[] = $x;
            return $x;
        });
        $this->ctx->eval('(log 1) (log 2) [(log 3) (log 4)]');
        $this->assertSame([1, 2, 3, 4], $log);
    }

    public function testInsideHigherOrderBuiltinsAndClosures(): void
    {
        $this->ctx->define('sq', fn ($x) => $x * $x);
        $this->assertSame([1, 4, 9], $this->ctx->eval('(map (fn [x] (sq x)) [1 2 3])'));
        $this->assertSame(14, $this->ctx->eval('(reduce (fn [a x] (+ a (sq x))) 0 [1 2 3])'));
        $this->ctx->eval('(defn sq-plus [x] (+ 1 (sq x)))');
        $this->assertSame(26, $this->ctx->call('sq-plus', 5));
    }

    public function testHostFunctionsShadowBuiltins(): void
    {
        $this->ctx->define('upcase', fn ($_) => 'mine');
        $this->assertSame('mine', $this->ctx->eval('(upcase "a")'));
        $this->assertTrue($this->ctx->undefine('upcase'));
        $this->assertFalse($this->ctx->undefine('upcase'));
        $this->assertSame('A', $this->ctx->eval('(upcase "a")'));
    }

    public function testUndefinedFunctionsAreGone(): void
    {
        $this->ctx->define('f', fn () => 1);
        $this->ctx->undefine('f');
        $this->assertSame([], $this->ctx->functions());
        $this->expectException(EvalException::class);
        $this->ctx->eval('(f)');
    }

    public function testRedefiningReplaces(): void
    {
        $this->ctx->define('f', fn () => 1);
        $this->ctx->define('f', fn () => 2);
        $this->assertSame(2, $this->ctx->eval('(f)'));
        $this->assertSame(['f'], $this->ctx->functions());
    }

    public function testCallReachesHostFunctions(): void
    {
        $this->ctx->define('add', fn ($a, $b) => $a + $b);
        $this->assertSame(5, $this->ctx->call('add', 2, 3));
    }

    public function testDefineValidation(): void
    {
        foreach ([[42, \TypeError::class], ['no_such_function_anywhere', \TypeError::class]] as [$fn, $class]) {
            try {
                $this->ctx->define('f', $fn);
                $this->fail('accepted a non-callable');
            } catch (\Throwable $e) {
                $this->assertInstanceOf($class, $e);
            }
        }
        try {
            $this->ctx->define('bad name', fn () => 1);
            $this->fail('accepted a bad name');
        } catch (\ValueError) {
            $this->addToAssertionCount(1);
        }
        $this->assertSame([], $this->ctx->functions());
    }

    // -- exceptions ---------------------------------------------------------

    public function testTheOriginalExceptionPropagates(): void
    {
        $error = new CustomException('custom failure', 42);
        $this->ctx->define('boom', function () use ($error) {
            throw $error;
        });
        try {
            $this->ctx->eval('(+ 1 (boom))');
            $this->fail('no exception');
        } catch (CustomException $e) {
            $this->assertSame($error, $e);
            $this->assertSame(42, $e->getCode());
            $this->assertNull($e->getPrevious());
        }
    }

    public function testExceptionsFromDeepInsideAScript(): void
    {
        $this->ctx->define('check', fn ($x) => $x < 3 ? $x : throw new \InvalidArgumentException("too big: $x"));
        $this->expectException(\InvalidArgumentException::class);
        $this->expectExceptionMessage('too big: 3');
        $this->ctx->eval('(map (fn [x] (check x)) [1 2 3 4])');
    }

    public function testErrorsThrownByTheCallable(): void
    {
        $this->ctx->define('div', fn ($a, $b) => intdiv($a, $b));
        $this->expectException(\DivisionByZeroError::class);
        $this->ctx->eval('(div 1 0)');
    }

    public function testTheContextIsUsableAfterACallbackFailure(): void
    {
        $this->ctx->define('boom', fn () => throw new \RuntimeException('boom'));
        $this->ctx->eval('(def kept 7)');
        try {
            $this->ctx->eval('(boom)');
            $this->fail('no exception');
        } catch (\RuntimeException) {
            $this->addToAssertionCount(1);
        }
        $this->assertSame(7, $this->ctx->eval('kept'));
    }

    public function testOnlyTheFailingCallAborts(): void
    {
        $calls = 0;
        $this->ctx->define('tick', function () use (&$calls) {
            if (++$calls === 3) {
                throw new \RuntimeException('stop');
            }
            return $calls;
        });
        try {
            $this->ctx->eval('(tick) (tick) (tick) (tick) (tick)');
            $this->fail('no exception');
        } catch (\RuntimeException) {
            $this->assertSame(3, $calls);
        }
    }

    // -- nesting and re-entrancy ---------------------------------------------

    public function testACallbackCanEvaluateInAnotherContext(): void
    {
        $other = new Context();
        $this->ctx->define('inner', fn ($src) => $other->eval($src));
        $this->assertSame(43, $this->ctx->eval('(+ 1 (inner "(* 6 7)"))'));
        $this->ctx->define('oneshot', fn ($src) => evaluate($src));
        $this->assertSame(10, $this->ctx->eval('(oneshot "(+ 4 6)")'));
    }

    public function testNestedCallbacksSeveralLevelsDeep(): void
    {
        $contexts = [];
        for ($i = 0; $i < 5; $i++) {
            $contexts[] = new Context();
        }
        for ($i = 0; $i < 4; $i++) {
            $inner = $contexts[$i + 1];
            $contexts[$i]->define('down', fn ($n) => $inner->eval('(down ' . ($n + 1) . ')'));
        }
        $contexts[4]->define('down', fn ($n) => $n);
        $this->assertSame(4, $contexts[0]->eval('(down 0)'));
    }

    public function testACallbackCannotReenterItsOwnContext(): void
    {
        $this->ctx->define('again', fn () => $this->ctx->eval('1'));
        try {
            $this->ctx->eval('(again)');
            $this->fail('no exception');
        } catch (ConcurrencyException $e) {
            $this->assertStringContainsString('already evaluating', $e->getMessage());
        }
        $this->assertSame(1, $this->ctx->eval('1'), 'the context recovers');
    }

    public function testACallbackCannotTouchItsOwnContext(): void
    {
        foreach ([
            fn () => $this->ctx->get('x'),
            fn () => $this->ctx->set('x', 1),
            fn () => $this->ctx->define('x', fn () => 1),
            fn () => $this->ctx->undefine('x'),
            fn () => $this->ctx->globals(),
        ] as $i => $body) {
            $this->ctx->define("touch$i", $body);
            try {
                $this->ctx->eval("(touch$i)");
                $this->fail("touch$i was allowed");
            } catch (ConcurrencyException) {
                $this->addToAssertionCount(1);
            }
        }
    }

    public function testManyCalls(): void
    {
        $this->ctx->define('inc', fn ($x) => $x + 1);
        $start = hrtime(true);
        $this->assertSame(5000, $this->ctx->eval('(reduce (fn [a x] (inc a)) 0 (range 5000))'));
        $this->assertLessThan(10.0, (hrtime(true) - $start) / 1e9);
    }

    public function testCallbacksGetCopies(): void
    {
        $this->ctx->define('mutate', function (array $list) {
            $list[] = 99;
            return null;
        });
        $this->assertSame([1, 2], $this->ctx->eval('(def xs [1 2]) (mutate xs) xs'));
    }

    public function testCallablesAreKeptAlive(): void
    {
        $make = function () {
            $secret = 'kept';
            return fn () => $secret;
        };
        $this->ctx->define('f', $make());
        gc_collect_cycles();
        $this->assertSame('kept', $this->ctx->eval('(f)'));
    }

    public function testContextsCanBeDestroyedRepeatedly(): void
    {
        $before = memory_get_usage();
        // Each context holds a value deeper than PHP or a plain recursive
        // drop could take apart on this stack.
        for ($i = 0; $i < 40; $i++) {
            $ctx = new Context(['functions' => ['f' => fn ($x) => $x]]);
            $ctx->eval('(def big (range 1000)) (def deep ' . self::deepListSource(1100) . ') (f 1)');
            unset($ctx);
        }
        gc_collect_cycles();
        $this->assertLessThan(8 * 1024 * 1024, memory_get_usage() - $before);
    }
}
