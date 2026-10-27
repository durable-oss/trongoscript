<?php

declare(strict_types=1);

namespace Trongoscript\Tests;

use Trongoscript\Context;
use Trongoscript\ResourceLimitException;

use function Trongoscript\evaluate;

final class LimitsTest extends TestCase
{
    private const RECURSE = '(defn down [n] (if (= n 0) 0 (+ 1 (down (- n 1)))))';

    private function assertLimited(string $src, array $options = []): void
    {
        try {
            evaluate($src, $options);
            $this->fail('no limit hit for ' . substr($src, 0, 60));
        } catch (ResourceLimitException) {
            $this->addToAssertionCount(1);
        }
    }

    public function testStepBudget(): void
    {
        $ctx = new Context(['limits' => ['max_steps' => 5000]]);
        $this->assertIsInt($ctx->eval('(reduce (fn [a x] (+ a x)) 0 (range 100))'));
        $this->expectException(ResourceLimitException::class);
        $this->expectExceptionMessage('5000');
        $ctx->eval('(reduce (fn [a x] (+ a x)) 0 (range 10000))');
    }

    public function testEachEvaluationGetsAFreshBudget(): void
    {
        $ctx = new Context(['limits' => ['max_steps' => 5000]]);
        for ($i = 0; $i < 20; $i++) {
            $ctx->eval('(reduce (fn [a x] (+ a x)) 0 (range 300))');
        }
        $this->addToAssertionCount(1);
    }

    public function testStrictPreset(): void
    {
        $this->assertSame(50, evaluate(self::RECURSE . ' (down 50)', ['limits' => 'strict']));
        $this->assertLimited(self::RECURSE . ' (down 200)', ['limits' => 'strict']);
        $this->assertLimited('(range 100000)', ['limits' => 'strict']);
    }

    public function testPresetWithOverrides(): void
    {
        $this->assertSame(200, evaluate(self::RECURSE . ' (down 200)', [
            'limits' => ['preset' => 'strict', 'max_call_depth' => 500, 'max_eval_depth' => 2000],
        ]));
    }

    public function testCallDepth(): void
    {
        $this->assertSame(1500, evaluate(self::RECURSE . ' (down 1500)'));
        $this->assertLimited(self::RECURSE . ' (down 3000)');
    }

    public function testDepthRequestsAreClampedToWhatTheStackSurvives(): void
    {
        $limits = ['limits' => ['max_call_depth' => 1_000_000_000, 'max_eval_depth' => 1_000_000_000]];
        $this->assertSame(3000, evaluate(self::RECURSE . ' (down 3000)', $limits));
        $this->assertLimited(self::RECURSE . ' (down 100000)', $limits);
    }

    public function testValueLength(): void
    {
        $this->assertSame(10, evaluate('(count (range 10))', ['limits' => ['max_value_len' => 10]]));
        $this->assertLimited('(range 11)', ['limits' => ['max_value_len' => 10]]);
    }

    public function testTotalAllocation(): void
    {
        $this->assertLimited('(reduce (fn [a x] (range 1000)) [] (range 1000))', ['limits' => ['max_total_alloc' => 100_000]]);
    }

    public function testUnlimitedPresetStillCapsDepth(): void
    {
        $this->assertLimited(self::RECURSE . ' (down 100000)', ['limits' => 'unlimited']);
    }

    public function testUsageIsRecordedForFailuresToo(): void
    {
        $ctx = new Context(['limits' => ['max_steps' => 1000]]);
        try {
            $ctx->eval('(defn f [] (f)) (f)');
        } catch (ResourceLimitException) {
        }
        $this->assertGreaterThanOrEqual(1000, $ctx->usage()['steps']);
    }
}
