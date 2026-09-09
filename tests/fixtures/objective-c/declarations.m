// Standalone declarations for native syntax and independent compiler checks.
// No platform SDK, linked Objective-C runtime or fixture execution is required.
@protocol Reading
- (int)value;
@end

__attribute__((objc_root_class))
@interface Counter <Reading> {
    int _value;
}
@property(nonatomic) int value;
- (int)add:(int)left to:(int)right;
+ (int)kind;
@end

@implementation Counter
@synthesize value = _value;
- (int)add:(int)left to:(int)right {
    return left + right;
}
+ (int)kind { return 1; }
@end

@interface Counter (Scaling)
- (int)twice;
@end

@implementation Counter (Scaling)
- (int)twice {
    return [self add:1 to:1];
}
@end

@interface Child : Counter
- (int)twice;
@end

@implementation Child
- (int)twice { return [super twice]; }
@end

int evaluate(Counter *counter) {
    return [counter add:2 to:[Counter kind]];
}
