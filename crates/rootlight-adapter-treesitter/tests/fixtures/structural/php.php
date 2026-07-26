<?php
/** Greets a café visitor. */
namespace Demo;

use Vendor\Formatter;

trait Greeting
{
    public function greet(string $name): string
    {
        return Formatter::format("olá", $name);
    }
}
