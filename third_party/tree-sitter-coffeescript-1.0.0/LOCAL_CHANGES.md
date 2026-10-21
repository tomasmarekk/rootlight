# Local CoffeeScript inputs

The complete pinned 63 upstream blobs remain under `upstream/`. Only inert upstream `.gitattributes` is stored as `.gitattributes.upstream`; its bytes are unchanged. The active scanner is separate from the pristine copy.

The active scanner uses a complete versioned wire format capped at 1024 bytes, fixed bounded state, canonical atomic deserialization, transactional declined scans, sticky typed status, cancellation and finite accepted external-scanner step credits. Embedded NUL and unmatched dedentation refuse rather than becoming EOF or truncated state. Memory enforcement remains unavailable.

The thread-affine Rust wrapper reserves generic probe storage fallibly in a Vec, then wraps that storage in a constant-size stable Rc allocation whose allocation is infallible. C owns an actual Rc count; each callback retains a temporary count before arbitrary safe probe code can drop its own guard. Callback and final-owner destructor panics are contained at the C boundary. Native reentry conflicts cannot reopen an operation. Borrowed probes cannot satisfy the static bound. Finish detaches before releasing the C count; forgetting a guard leaks valid ownership. No hard or accounted memory guarantee is claimed.

SCANNER.patch retains the earlier scanner correction. CALLBACK_OWNER.patch records the separate generic owner/reentry correction; the old Cancellation-only nine-test and C13 historical capsules remain unchanged and do not qualify this changed bridge or generic wrapper. The first-party adapter contains no new unsafe code.

Build inputs are the unchanged generated parser, separate active scanner and status unit. No generator or network fetch runs during compilation. Source-only structural lowering does not infer call targets, types, runtime exports, inheritance or reads.
