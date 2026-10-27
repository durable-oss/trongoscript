<?php

declare(strict_types=1);

namespace Trongoscript\Tests;

/**
 * Engine-level failures inside host functions, each in a child process:
 * fatal errors unwind with `longjmp`, which must never jump over the
 * evaluator thread, and `exit()` must still end the process normally.
 */
final class FatalTest extends TestCase
{
    public function testTheChildProcessHarnessWorks(): void
    {
        [$code, $out] = self::runPhp('echo Trongoscript\evaluate("(+ 1 2)");');
        $this->assertSame(0, $code);
        $this->assertSame('3', $out);
    }

    public function testMemoryLimitInsideACallback(): void
    {
        [$code, $out, $err] = self::runPhp(<<<'PHP'
            register_shutdown_function(function () { echo "shutdown ran\n"; });
            $ctx = new Trongoscript\Context();
            $ctx->define('hog', function () { $a = []; while (true) { $a[] = str_repeat('x', 1 << 20); } });
            echo "before\n";
            $ctx->eval('(hog)');
            echo "not reached\n";
            PHP, ['memory_limit' => '64M', 'display_errors' => 'stderr']);
        $this->assertSame(255, $code, $err);
        $this->assertStringContainsString('Allowed memory size', $out . $err);
        $this->assertStringContainsString("before\n", $out);
        $this->assertStringContainsString('shutdown ran', $out);
        $this->assertStringNotContainsString('not reached', $out);
    }

    public function testTimeLimitInsideACallback(): void
    {
        [$code, $out, $err] = self::runPhp(<<<'PHP'
            set_time_limit(1);
            $ctx = new Trongoscript\Context();
            $ctx->define('spin', function () { while (true) {} });
            $ctx->eval('(spin)');
            echo "not reached\n";
            PHP, ['display_errors' => 'stderr']);
        $this->assertSame(255, $code, $err);
        $this->assertStringContainsString('Maximum execution time', $out . $err);
        $this->assertStringNotContainsString('not reached', $out);
    }

    public function testExitInsideACallback(): void
    {
        [$code, $out, $err] = self::runPhp(<<<'PHP'
            register_shutdown_function(function () { echo "shutdown ran\n"; });
            $ctx = new Trongoscript\Context();
            $ctx->define('quit', function () { echo "bye\n"; exit(3); });
            $ctx->eval('(+ 1 (quit))');
            echo "not reached\n";
            PHP);
        $this->assertSame(3, $code, $err);
        $this->assertSame("bye\nshutdown ran\n", $out);
    }

    public function testUserFatalErrorInsideACallback(): void
    {
        [$code, $out, $err] = self::runPhp(<<<'PHP'
            $ctx = new Trongoscript\Context();
            $ctx->define('die', function () { trigger_error('user fatal', E_USER_ERROR); });
            $ctx->eval('(die)');
            echo "not reached\n";
            PHP, ['display_errors' => 'stderr', 'error_reporting' => (string) E_ALL]);
        $this->assertSame(255, $code, $err);
        $this->assertStringContainsString('user fatal', $out . $err);
        $this->assertStringNotContainsString('not reached', $out);
    }

    public function testAnUncaughtEvaluationErrorEndsTheProcessCleanly(): void
    {
        [$code, $out, $err] = self::runPhp('Trongoscript\evaluate("(+ 1");', ['display_errors' => 'stderr']);
        $this->assertSame(255, $code);
        $this->assertStringContainsString('Trongoscript\ParseException', $out . $err);
    }

    public function testAFatalErrorDoesNotLeaveTheContextBroken(): void
    {
        // The fatal happens inside a nested evaluation; the shutdown
        // function then uses the outer context, which must have been
        // released when the evaluator unwound.
        [$code, $out, $err] = self::runPhp(<<<'PHP'
            $ctx = new Trongoscript\Context();
            register_shutdown_function(function () use ($ctx) {
                echo "after: ", $ctx->eval('(+ 40 2)'), "\n";
            });
            $ctx->define('hog', function () { $a = []; while (true) { $a[] = str_repeat('x', 1 << 20); } });
            $ctx->eval('(hog)');
            PHP, ['memory_limit' => '64M', 'display_errors' => 'stderr']);
        $this->assertSame(255, $code, $err);
        $this->assertStringContainsString('after: 42', $out, $err);
    }

    public function testDeepRecursionDoesNotTouchThePhpStack(): void
    {
        // Run with PHP's own stack-size guard tight: evaluation happens on
        // the extension's thread, not PHP's.
        [$code, $out, $err] = self::runPhp(
            'echo Trongoscript\evaluate("(defn down [n] (if (= n 0) 0 (+ 1 (down (- n 1))))) (down 2000)");',
        );
        $this->assertSame(0, $code, $err);
        $this->assertSame('2000', $out);
    }
}
