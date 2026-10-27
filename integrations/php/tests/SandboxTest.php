<?php

declare(strict_types=1);

namespace Trongoscript\Tests;

use Trongoscript\Context;
use Trongoscript\SandboxException;

final class SandboxTest extends TestCase
{
    public function testDirectoryRoot(): void
    {
        self::withTree([
            'main.trongo' => '{:db (include "lib/db.trongo")}',
            'lib/db.trongo' => '{:host "localhost"}',
            'notes.txt' => "hello\n",
        ], function (string $dir) {
            $ctx = new Context(['root' => $dir]);
            $this->assertSame(['db' => ['host' => 'localhost']], $ctx->evalFile('main.trongo'));
            $this->assertSame("hello\n", $ctx->eval('(read-file "notes.txt")'));
            $this->assertSame("hello\n", $ctx->eval('(read-file "/notes.txt")'));
            $this->assertSame([['dir?' => false, 'name' => 'db.trongo']], $ctx->eval('(list-files "lib")'));
        });
    }

    public function testMounts(): void
    {
        self::withTree(['root/a.txt' => 'root', 'vendor/b.txt' => 'vendor'], function (string $dir) {
            $ctx = new Context(['root' => "$dir/root", 'mounts' => ['/vendor' => "$dir/vendor"]]);
            $this->assertSame(['root', 'vendor'], $ctx->eval('[(read-file "a.txt") (read-file "/vendor/b.txt")]'));
        });
    }

    public function testEscapesAreRefused(): void
    {
        self::withTree(['root/a.txt' => 'a', 'secret.txt' => 'secret'], function (string $dir) {
            $ctx = new Context(['root' => "$dir/root"]);
            foreach (['../secret.txt', '/../secret.txt', '/etc/passwd', "$dir/secret.txt"] as $path) {
                try {
                    $ctx->eval('(read-file ' . json_encode($path) . ')');
                    $this->fail("read $path");
                } catch (SandboxException) {
                    $this->addToAssertionCount(1);
                }
            }
        });
    }

    public function testSymlinksOutOfTheSandboxAreRefused(): void
    {
        self::withTree(['root/a.txt' => 'a', 'secret.txt' => 'secret'], function (string $dir) {
            symlink("$dir/secret.txt", "$dir/root/link.txt");
            $ctx = new Context(['root' => "$dir/root"]);
            $this->expectException(SandboxException::class);
            $ctx->eval('(read-file "link.txt")');
        });
    }

    public function testInMemoryFiles(): void
    {
        $ctx = new Context(['files' => [
            'main.trongo' => '(include "lib/x.trongo")',
            '/lib/x.trongo' => '{:x 1}',
            'data/notes.txt' => '☃',
        ]]);
        $this->assertSame(['x' => 1], $ctx->eval('(include "main.trongo")'));
        $this->assertSame('☃', $ctx->eval('(read-file "data/notes.txt")'));
        $this->assertSame([['dir?' => false, 'name' => 'x.trongo']], $ctx->eval('(list-files "lib")'));
        foreach (['(read-file "missing")', '(read-file "../main.trongo")'] as $src) {
            try {
                $ctx->eval($src);
                $this->fail($src);
            } catch (SandboxException) {
                $this->addToAssertionCount(1);
            }
        }
    }

    public function testIncludeCycles(): void
    {
        $ctx = new Context(['files' => ['a.trongo' => '(include "b.trongo")', 'b.trongo' => '(include "a.trongo")']]);
        $this->expectException(SandboxException::class);
        $this->expectExceptionMessage('cycle');
        $ctx->evalFile('a.trongo');
    }

    public function testFileSizeLimit(): void
    {
        $ctx = new Context(['files' => ['big.txt' => str_repeat('x', 100)], 'sandbox_limits' => ['max_file_bytes' => 10]]);
        $this->expectException(SandboxException::class);
        $this->expectExceptionMessage('too large');
        $ctx->eval('(read-file "big.txt")');
    }

    public function testIncludeDepthLimit(): void
    {
        $chain = [];
        for ($i = 0; $i <= 5; $i++) {
            $chain["f$i.trongo"] = $i === 5 ? '1' : '(include "f' . ($i + 1) . '.trongo")';
        }
        $this->assertSame(1, (new Context(['files' => $chain]))->evalFile('f0.trongo'));
        $this->expectException(SandboxException::class);
        (new Context(['files' => $chain, 'sandbox_limits' => ['max_include_depth' => 3]]))->evalFile('f0.trongo');
    }

    public function testIncludeSharesDefinitionsWithTheContext(): void
    {
        $ctx = new Context(['files' => ['lib.trongo' => '(defn triple [x] (* 3 x))']]);
        $ctx->evalFile('lib.trongo');
        $this->assertSame(9, $ctx->eval('(triple 3)'));
    }
}
