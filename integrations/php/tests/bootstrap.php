<?php

declare(strict_types=1);

if (!extension_loaded('trongoscript')) {
    fwrite(STDERR, "The trongoscript extension is not loaded. Run `composer test`, or pass\n"
        . "-d extension=target/debug/libtrongoscript.so to php.\n");
    exit(1);
}

require __DIR__ . '/../vendor/autoload.php';
