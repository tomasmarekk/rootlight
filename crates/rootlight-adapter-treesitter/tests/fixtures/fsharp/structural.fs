// F# structural selector fixture covers written declarations without inferred relationships.
// Recursive siblings expose separate body ownership despite one native parent.
module Sample.Root

open System
open type System.Math

module Nested =
    let value = 3
    let ``value with spaces`` = value
    let add x y = x + y
    let ( /+ ) x y = x / y
    let rec first n = n + 1
    and second n = n + 2

type Record = { Name: string; mutable Count: int }
type Choice = | Empty | Named of name: string
type Alias = string

type Container(seed: int) =
    member this.Read amount = seed + amount
    member this.Value = seed
    new () = Container(0)
