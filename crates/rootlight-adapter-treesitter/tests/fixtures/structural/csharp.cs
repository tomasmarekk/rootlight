/// Greets a café visitor.
using System;

namespace Demo;

public sealed class Visitor
{
    public string Greet(string name)
    {
        return string.Concat("olá", name);
    }
}
