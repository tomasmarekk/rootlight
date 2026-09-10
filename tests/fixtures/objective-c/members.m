// Written ivars and method parameters without a platform SDK or linked runtime.
// Pointer and bit-field declarators remain distinct from callback type parameters.
enum { PaddingWidth = 2 };
__attribute__((objc_root_class))
@interface Members {
@public
    int first, second;
    Members *next;
    int (*callback)(int argument);
    int (^handler)(int blockArgument);
    int slots[4];
    unsigned int flags : 3;
    unsigned int : PaddingWidth;
    _Atomic(int) state;
}
@property(nonatomic) int first;
- (int)combine:(int)left with:(int)right;
- (int)legacy:(int)value, int extra;
+ (int)from:(int)input;
@end

@implementation Members
@synthesize first;
- (int)combine:(int)left with:(int)right {
    return left + right;
}
- (int)legacy:(int)value, int extra {
    return value + extra;
}
+ (int)from:(int)input {
    return input;
}
@end
