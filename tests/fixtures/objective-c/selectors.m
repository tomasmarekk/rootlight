// Empty selector components are valid written Objective-C method names.
// Class and instance methods may share a selector without sharing dispatch.
__attribute__((objc_root_class))
@interface Selectors
- (int):(int)first :(int)second;
- (int)perform:(int)first :(int)second;
- (int)value;
+ (int)value;
@end

int invoke(Selectors *receiver) {
    return [receiver :1 :2] + [receiver perform:1 :2]
        + [receiver value] + [Selectors value];
}
