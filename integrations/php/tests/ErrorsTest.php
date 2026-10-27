<?php

declare(strict_types=1);

namespace Trongoscript\Tests;

use PHPUnit\Framework\Attributes\DataProvider;
use Trongoscript\CallbackException;
use Trongoscript\ConcurrencyException;
use Trongoscript\Context;
use Trongoscript\EvalException;
use Trongoscript\Exception;
use Trongoscript\ParseException;
use Trongoscript\ResourceLimitException;
use Trongoscript\SandboxException;

use function Trongoscript\embed;
use function Trongoscript\embed_yaml;
use function Trongoscript\evaluate;
use function Trongoscript\render;

final class ErrorsTest extends TestCase
{
    public function testHierarchy(): void
    {
        $this->assertTrue(is_subclass_of(Exception::class, \Exception::class));
        foreach ([ParseException::class, EvalException::class, SandboxException::class,
            ResourceLimitException::class, CallbackException::class, ConcurrencyException::class] as $class) {
            $this->assertTrue(is_subclass_of($class, Exception::class), $class);
        }
    }

    public static function parseErrors(): iterable
    {
        foreach (['(+ 1', '[1 2', '{:a}', ')', '"unterminated'] as $src) {
            yield $src => [fn () => evaluate($src)];
        }
        yield 'template' => [fn () => render('<%= (+ 1 %>')];
        yield 'yaml' => [fn () => embed_yaml('a: [1')];
        yield 'snippet' => [fn () => embed(['a' => '!trongo ('])];
    }

    #[DataProvider('parseErrors')]
    public function testParseErrors(\Closure $fn): void
    {
        $this->expectException(ParseException::class);
        $fn();
    }

    public function testEvalErrors(): void
    {
        try {
            evaluate('undefined-thing');
            $this->fail('no exception');
        } catch (EvalException $e) {
            $this->assertStringContainsString('undefined-thing', $e->getMessage());
        }
        foreach (['(no-such-function 1)', '(+ 1 "a")', '((fn [x] x))'] as $src) {
            try {
                evaluate($src);
                $this->fail("no exception for $src");
            } catch (EvalException) {
                $this->addToAssertionCount(1);
            }
        }
    }

    public function testSandboxErrorsWithoutASandbox(): void
    {
        foreach (['(read-file "x")', '(include "x")', '(list-files ".")'] as $src) {
            try {
                evaluate($src);
                $this->fail("no exception for $src");
            } catch (SandboxException $e) {
                $this->assertMatchesRegularExpression('/sandbox|not configured/', $e->getMessage());
            }
        }
    }

    public function testResourceLimitErrors(): void
    {
        $cases = [
            ['(defn f [n] (f n)) (f 1)', ['limits' => ['max_steps' => 10_000]]],
            ['(defn f [n] (+ 1 (f n))) (f 1)', ['limits' => 'strict']],
            ['(range 100)', ['limits' => ['max_value_len' => 10]]],
            [str_repeat('[', 50_000) . '1' . str_repeat(']', 50_000), []],
        ];
        foreach ($cases as [$src, $opts]) {
            try {
                evaluate($src, $opts);
                $this->fail('no exception for ' . substr($src, 0, 40));
            } catch (ResourceLimitException) {
                $this->addToAssertionCount(1);
            }
        }
    }

    public function testAFailedEvaluationLeavesTheContextUsable(): void
    {
        $ctx = new Context(['limits' => ['max_steps' => 10_000]]);
        $ctx->eval('(def kept 1)');
        foreach (['(', '(nope)', '(defn f [] (f)) (f)'] as $src) {
            try {
                $ctx->eval($src);
                $this->fail("no exception for $src");
            } catch (Exception) {
                $this->addToAssertionCount(1);
            }
        }
        $this->assertSame(1, $ctx->eval('kept'));
    }

    public static function badOptions(): iterable
    {
        yield 'root and files' => [['root' => '.', 'files' => ['a' => '']], \ValueError::class];
        yield 'mounts without root' => [['mounts' => ['/x' => '.']], \ValueError::class];
        yield 'unknown option' => [['nope' => 1], \ValueError::class];
        yield 'marker outside embed' => [['prefix' => '='], \ValueError::class];
        yield 'unknown preset' => [['limits' => 'loose'], \ValueError::class];
        yield 'unknown limit' => [['limits' => ['max_stepz' => 1]], \ValueError::class];
        yield 'unknown sandbox limit' => [['sandbox_limits' => ['max_files' => 1]], \ValueError::class];
        yield 'bad global name' => [['globals' => ['a b' => 1]], \ValueError::class];
        yield 'empty files' => [['files' => []], \ValueError::class];
        yield 'escaping file' => [['files' => ['../x' => '']], \ValueError::class];
        yield 'negative seed' => [['seed' => -1], \ValueError::class];
        yield 'negative limit' => [['limits' => ['max_steps' => -1]], \ValueError::class];
        yield 'limits type' => [['limits' => 5], \TypeError::class];
        yield 'globals type' => [['globals' => 'x'], \TypeError::class];
        yield 'globals list' => [['globals' => [1, 2]], \TypeError::class];
        yield 'file contents' => [['files' => ['a' => 1]], \TypeError::class];
        yield 'seed type' => [['seed' => '1'], \TypeError::class];
        yield 'limit type' => [['limits' => ['max_steps' => 'many']], \TypeError::class];
        yield 'objects type' => [['objects' => 1], \TypeError::class];
        yield 'function not callable' => [['functions' => ['f' => 'no_such_function_anywhere']], \TypeError::class];
        yield 'options list' => [[1, 2], \TypeError::class];
        yield 'non-UTF-8 option key' => [["\xFF" => 1], \ValueError::class];
        yield 'non-UTF-8 global name' => [['globals' => ["\xFF" => 1]], \ValueError::class];
    }

    #[DataProvider('badOptions')]
    public function testOptionErrors(array $options, string $class): void
    {
        $this->expectException($class);
        new Context($options);
    }

    public function testPresets(): void
    {
        foreach (['default', 'strict', 'unlimited'] as $preset) {
            $this->assertSame(3, evaluate('(+ 1 2)', ['limits' => $preset]));
            $this->assertSame(3, evaluate('(+ 1 2)', ['limits' => ['preset' => $preset]]));
        }
    }

    public function testBadSandboxRoots(): void
    {
        try {
            new Context(['root' => '/definitely/not/here']);
            $this->fail('no exception');
        } catch (SandboxException $e) {
            $this->assertStringContainsString('/definitely/not/here', $e->getMessage());
        }
        $dir = sys_get_temp_dir();
        foreach ([['relative' => $dir], ['/x' => '/definitely/not/here']] as $mounts) {
            try {
                new Context(['root' => $dir, 'mounts' => $mounts]);
                $this->fail('no exception');
            } catch (SandboxException) {
                $this->addToAssertionCount(1);
            }
        }
    }

    public function testInvalidGlobalNames(): void
    {
        $ctx = new Context();
        foreach (['', 'a b', '(x)', 'x;y'] as $name) {
            try {
                $ctx->set($name, 1);
                $this->fail("accepted \"$name\"");
            } catch (\ValueError) {
                $this->addToAssertionCount(1);
            }
        }
    }

    public function testMessagesAreNotEmpty(): void
    {
        try {
            evaluate('(+ 1 "a")');
        } catch (EvalException $e) {
            $this->assertNotSame('', $e->getMessage());
            $this->assertSame(0, $e->getCode());
        }
    }
}
