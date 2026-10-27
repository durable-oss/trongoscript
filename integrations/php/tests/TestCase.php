<?php

declare(strict_types=1);

namespace Trongoscript\Tests;

use PHPUnit\Framework\TestCase as Base;

abstract class TestCase extends Base
{
    /** An array nested `$depth` levels deep around `$leaf`. */
    protected static function nested(int $depth, mixed $leaf = 1): mixed
    {
        for ($i = 0; $i < $depth; $i++) {
            $leaf = [$leaf];
        }
        return $leaf;
    }

    /** Source building a list `$depth` levels deep, equal to nested($depth - 1, []). */
    protected static function deepListSource(int $depth): string
    {
        return '(reduce (fn [acc x] [acc]) [] (range ' . ($depth - 1) . '))';
    }

    /**
     * Run `$fn` with a temporary directory holding `$files` (path => contents).
     */
    protected static function withTree(array $files, callable $fn): mixed
    {
        $dir = sys_get_temp_dir() . '/trongo-' . bin2hex(random_bytes(6));
        mkdir($dir);
        try {
            foreach ($files as $path => $contents) {
                $full = "$dir/$path";
                if (!is_dir(dirname($full))) {
                    mkdir(dirname($full), 0777, true);
                }
                file_put_contents($full, $contents);
            }
            return $fn($dir);
        } finally {
            exec('rm -rf ' . escapeshellarg($dir));
        }
    }

    /**
     * Run PHP code in a child process with the extension loaded.
     *
     * @return array{int, string, string} exit code, stdout, stderr
     */
    protected static function runPhp(string $code, array $ini = []): array
    {
        $extension = getenv('TRONGOSCRIPT_EXT') ?: realpath(__DIR__ . '/../target/debug/libtrongoscript.so');
        self::assertNotFalse($extension, 'build the extension first');
        $args = [PHP_BINARY, '-d', "extension=$extension"];
        foreach ($ini as $k => $v) {
            array_push($args, '-d', "$k=$v");
        }
        array_push($args, '-r', $code);
        // Without PHP's own allocator, which `memcheck` turns off, there is
        // no memory_limit; the child must have it.
        $env = getenv();
        unset($env['USE_ZEND_ALLOC']);
        $proc = proc_open($args, [1 => ['pipe', 'w'], 2 => ['pipe', 'w']], $pipes, null, $env);
        self::assertIsResource($proc);
        $out = stream_get_contents($pipes[1]);
        $err = stream_get_contents($pipes[2]);
        fclose($pipes[1]);
        fclose($pipes[2]);
        return [proc_close($proc), $out, $err];
    }
}
