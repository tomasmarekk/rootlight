// F# signature selector fixture retains authored contracts separately from implementations.
// Arrow-bearing val types are not evidence of an executable function body.
module Sample.Root

open System
open type System.Math

module Nested =
    val value: int
    val ``value with spaces``: int
    val add: int -> int -> int

type Record = { Name: string; mutable Count: int }
type Choice = | Empty | Named of name: string
type Alias = string

type Container =
    new: seed:int -> Container
    member Read: amount:int -> int
    member Value: int with get
