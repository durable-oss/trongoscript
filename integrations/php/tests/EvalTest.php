<?php

declare(strict_types=1);

namespace Trongoscript\Tests;

use Trongoscript\Context;
use Trongoscript\EvalException;

use function Trongoscript\dump;
use function Trongoscript\evaluate;
use function Trongoscript\evaluate_file;
use function Trongoscript\render;

final class EvalTest extends TestCase
{
    public function testScalars(): void
    {
        $this->assertSame(42, evaluate('42'));
        $this->assertSame(-7, evaluate('-7'));
        $this->assertSame(3.5, evaluate('3.5'));
        $this->assertTrue(evaluate('true'));
        $this->assertFalse(evaluate('false'));
        $this->assertNull(evaluate('nil'));
        $this->assertSame('s', evaluate('"s"'));
        $this->assertSame('kw', evaluate(':kw'));
    }

    public function testArithmetic(): void
    {
        $this->assertSame(3, evaluate('(+ 1 2)'));
        $this->assertSame(2, evaluate('(/ 4 2)'));
        $this->assertSame(0.5, evaluate('(/ 1 2)'));
    }

    public function testCollections(): void
    {
        $this->assertSame([1, [2, [3]], []], evaluate('[1 [2 [3]] []]'));
        $this->assertSame(['a' => 1, 'b' => ['c' => [true]]], evaluate('{:a 1 :b {:c [true]}}'));
        $this->assertSame([], evaluate('{}'));
    }

    public function testMapKeysComeBackSorted(): void
    {
        $this->assertSame(['a', 'b', 'c'], array_keys(evaluate('{:c 1 :a 2 :b 3}')));
    }

    public function testValueOfTheLastForm(): void
    {
        $this->assertSame(3, evaluate('1 2 3'));
        $this->assertNull(evaluate(''));
        $this->assertNull(evaluate("  ; a comment\n"));
    }

    public function testUtf8(): void
    {
        $this->assertSame('héllo ☃ 𝄞', evaluate('"héllo ☃ 𝄞"'));
        $this->assertSame('HÉLLO', evaluate('(upcase "héllo")'));
    }

    public function testInterpolation(): void
    {
        $this->assertSame('hi ada', evaluate('"hi #{name}"', ['globals' => ['name' => 'ada']]));
    }

    public function testSourceMustBeUtf8Text(): void
    {
        $this->expectException(\ValueError::class);
        $this->expectExceptionMessage('not valid UTF-8');
        evaluate("\"\xFF\"");
    }

    public function testSourceMustBeAString(): void
    {
        $this->expectException(\TypeError::class);
        evaluate(42);
    }

    public function testOneShotCallsShareNothing(): void
    {
        evaluate('(def x 1)');
        $this->expectException(EvalException::class);
        evaluate('x');
    }

    public function testContextKeepsDefinitions(): void
    {
        $ctx = new Context();
        $ctx->eval('(def base 40)');
        $ctx->eval('(defn add-base [n] (+ base n))');
        $ctx->eval('(defmacro twice [x] `(* 2 ~x))');
        $this->assertSame(42, $ctx->eval('(add-base 2)'));
        $this->assertSame(84, $ctx->eval('(twice (add-base 2))'));
    }

    public function testSeed(): void
    {
        $roll = fn ($seed) => evaluate(
            '(use rng) [(rand-int 0 1000000) (rand-int 0 1000000)]',
            $seed === null ? [] : ['seed' => $seed],
        );
        $this->assertSame($roll(5), $roll(5));
        $this->assertNotSame($roll(5), $roll(6));
        $this->assertSame($roll(0), $roll(null), 'the default seed is 0');
    }

    public function testGlobalsOption(): void
    {
        $ctx = new Context(['globals' => ['env' => 'prod', 'port' => 443, 'tags' => ['a', 'b']]]);
        $this->assertSame(['prod', 443, ['a', 'b']], $ctx->eval('[env port tags]'));
    }

    public function testGlobalAccessors(): void
    {
        $ctx = new Context();
        $ctx->set('b', ['x' => [1, 2]]);
        $ctx->set('a', 1);
        $this->assertSame(['x' => [1, 2]], $ctx->get('b'));
        $this->assertSame(1, $ctx->eval('a'));
        $this->assertTrue($ctx->has('a'));
        $this->assertFalse($ctx->has('zzz'));
        $this->assertNull($ctx->get('zzz'));
        $this->assertSame(['a', 'b'], array_keys($ctx->globals()));
        $this->assertTrue($ctx->remove('a'));
        $this->assertFalse($ctx->remove('a'));
        $this->assertFalse($ctx->has('a'));
    }

    public function testNullGlobalsExist(): void
    {
        $ctx = new Context(['globals' => ['n' => null]]);
        $this->assertTrue($ctx->has('n'));
        $this->assertNull($ctx->eval('n'));
    }

    public function testDefinitionsAreVisibleAsGlobals(): void
    {
        $ctx = new Context();
        $ctx->eval('(def greeting "hi")');
        $this->assertSame('hi', $ctx->get('greeting'));
    }

    public function testObjectsMode(): void
    {
        $ctx = new Context(['objects' => true, 'globals' => ['g' => 1]]);
        $v = $ctx->eval('{:a {:b [{:c "d"}]} :e {}}');
        $this->assertInstanceOf(\stdClass::class, $v);
        $this->assertInstanceOf(\stdClass::class, $v->a);
        $this->assertSame('d', $v->a->b[0]->c);
        $this->assertEquals(new \stdClass(), $v->e);
        $this->assertSame(['g' => 1], $ctx->globals(), 'globals() is a listing, always an array');
    }

    public function testCallByName(): void
    {
        $ctx = new Context();
        $ctx->eval('(defn greet [who punct] (str "hi " who punct))');
        $this->assertSame('hi bo!', $ctx->call('greet', 'bo', '!'));
        $this->assertSame('ABC', $ctx->call('upcase', 'abc'));
        $this->assertSame(['x' => 1], $ctx->call('merge', ['x' => 0], ['x' => 1]));
    }

    public function testCallUnknownFunction(): void
    {
        $this->expectException(EvalException::class);
        (new Context())->call('missing');
    }

    public function testCallWithAnInvalidName(): void
    {
        $this->expectException(\ValueError::class);
        (new Context())->call('not a name');
    }

    public function testCallArgumentsAreData(): void
    {
        $ctx = new Context();
        $ctx->eval('(defn ident [x] x)');
        foreach (['(boom)', '#{x}', '${x}', ['k' => '(boom)'], [1, null, 'x'], [], -0.25, PHP_INT_MAX, PHP_INT_MIN] as $v) {
            $this->assertSame($v, $ctx->call('ident', $v));
        }
    }

    public function testCallAFunctionValue(): void
    {
        $ctx = new Context();
        $adder = $ctx->eval('(let [k 10] (fn [x] (+ x k)))');
        $this->assertSame(15, $ctx->call($adder, 5));
        $ctx->eval('(defn mul [a b] (* a b))');
        $this->assertSame(12, $ctx->call($ctx->get('mul'), 3, 4));
    }

    public function testFunctionValuesInObjectsMode(): void
    {
        $ctx = new Context(['objects' => true]);
        $f = $ctx->eval('(fn [x] (* x x))');
        $this->assertInstanceOf(\stdClass::class, $f);
        $this->assertSame(49, $ctx->call($f, 7));
    }

    public function testCallingANonFunctionValue(): void
    {
        $this->expectException(EvalException::class);
        (new Context())->call(['not' => 'a function']);
    }

    public function testEvalFile(): void
    {
        $ctx = new Context(['files' => [
            'app/main.trongo' => '{:part (include "part.trongo")}',
            'app/part.trongo' => '[1 2]',
        ]]);
        $this->assertSame(['part' => [1, 2]], $ctx->evalFile('app/main.trongo'));
        $this->assertSame(['part' => [1, 2]], $ctx->evalFile('/app/main.trongo'));
        $this->assertSame([1, 2], evaluate_file('x.trongo', ['files' => ['x.trongo' => '[1 2]']]));
    }

    public function testRender(): void
    {
        $this->assertSame(
            'Hello ADA! n=3',
            render('Hello <%= (upcase name) %>!<% (def n 3) %> n=<%= n %>', ['globals' => ['name' => 'Ada']]),
        );
        $this->assertSame('plain ☃', render('plain ☃'));
        $this->assertSame('', render(''));
    }

    public function testRenderKeepsDefinitionsInAContext(): void
    {
        $ctx = new Context();
        $ctx->render('<% (def x 5) %>');
        $this->assertSame(5, $ctx->eval('x'));
    }

    public function testDumpRoundTrips(): void
    {
        $value = ['a' => [1, 2.5, 'q"uote', '#{x}', '$', "\n\t", null, true, false], 'b' => ['c' => []]];
        foreach ([false, true] as $pretty) {
            $src = dump($value, $pretty);
            $this->assertSame($value, evaluate($src), $src);
        }
        $this->assertStringContainsString("\n", dump(['a' => ['b' => [1]]], true));
        $this->assertSame('nil', dump(null));
    }

    public function testDumpRejectsUnsupportedValues(): void
    {
        $this->expectException(\TypeError::class);
        dump(new \DateTime());
    }

    public function testUsage(): void
    {
        $ctx = new Context();
        $ctx->eval('(defn down [n] (if (= n 0) 0 (+ 1 (down (- n 1))))) (down 50)');
        $big = $ctx->usage();
        $keys = array_keys($big);
        sort($keys);
        $this->assertSame(['allocated', 'peak_call_depth', 'peak_eval_depth', 'steps'], $keys);
        $this->assertGreaterThan(100, $big['steps']);
        $this->assertGreaterThanOrEqual(50, $big['peak_call_depth']);
        $ctx->eval('1');
        $this->assertLessThan(10, $ctx->usage()['steps']);
        $this->assertLessThan(50, $ctx->usage()['peak_call_depth']);
    }

    public function testContextIsFinal(): void
    {
        $this->assertTrue(class_exists(Context::class));
        $this->assertTrue(function_exists('Trongoscript\evaluate'));
    }
}
