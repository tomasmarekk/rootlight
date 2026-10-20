/// Synthetic example covering all Q# language constructs.
/// Used to verify tree-sitter-qsharp parsing and highlighting.

namespace Sample.Showcase {
    import Std.Math.*;
    import Std.Diagnostics.Fact;

    export RunDemo, Pair;

    // --- Struct declaration ---
    struct Pair {
        First : Int,
        Second : Double,
    }

    // --- Newtype declaration ---
    newtype PositiveInt = Int;

    // --- Function with generics and type constraints ---
    function Identity<'T>(value : 'T) : 'T {
        value
    }

    // --- Operation with functors ---
    operation Entangle(q1 : Qubit, q2 : Qubit) : Unit is Adj + Ctl {
        H(q1);
        CNOT(q1, q2);
    }

    // --- Specializations ---
    operation CustomAdj(q : Qubit) : Unit is Adj {
        body ... {
            Rx(PI() / 4.0, q);
        }
        adjoint ... {
            Rx(-PI() / 4.0, q);
        }
    }

    // --- Main entry point ---
    @EntryPoint()
    operation RunDemo() : Result[] {
        // Qubit allocation
        use register = Qubit[3];
        use aux = Qubit();

        // Mutable variable with type annotation
        mutable count : Int = 0;

        // For loop with step range
        for i in 0..2..10 {
            set count += 1;
        }

        // Immutable binding
        let pair = new Pair { First = count, Second = 3.14 };
        let updated = new Pair { ...pair, Second = 2.718 };

        // String interpolation
        Message($"Count is {count}, pair.First is {pair.First}");

        // Conditional branching
        if count > 5 {
            Message("High");
        } elif count > 2 {
            Message("Medium");
        } else {
            Message("Low");
        }

        // While loop
        mutable n = 10;
        while n > 0 {
            set n -= 1;
        }

        // Repeat-until-fixup
        use target = Qubit();
        repeat {
            H(target);
        } until M(target) == Zero
        fixup {
            Reset(target);
        }

        // Within-apply conjugation
        within {
            H(aux);
        } apply {
            CNOT(aux, register[0]);
        }

        // Partial application and lambda
        let addOne = Identity(1, _);
        let double = (x) -> x * 2;

        // Copy-and-update expression
        let arr = [1, 2, 3, 4, 5];
        let modified = arr w/ 0 <- 99;

        // Ternary expression
        let label = count > 3 ? "many" | "few";

        // Array repeat
        let zeros = [0, size = 10];

        // Open-ended range slicing
        let slice = arr[1...];
        let rev = arr[...-1...];

        // Bitwise and logical operators
        let bits = 0b1010 &&& 0xFF;
        let check = true and false or not true;

        // BigInt and imaginary literals
        let big = 42L;
        let complex = 1.0 + 2.5i;

        // Functor application
        Adjoint Entangle(register[0], register[1]);
        Controlled Entangle([aux], (register[0], register[1]));

        // Unwrap
        let pos = PositiveInt(42);
        let raw = pos!;

        // Discard and hole
        let _ = 0;
        let f = Entangle(_, _);

        // Measurement
        return [M(register[0]), M(register[1]), M(register[2])];
    }
}
