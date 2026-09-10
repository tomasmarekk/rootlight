// Forward type declarations remain source occurrences, not complete definitions.
// Repeated names and generic parameters exercise independently written identities.
@class Earlier, Later;
@protocol Readable, Writable;
@class Box<__covariant Item>, Earlier;
@protocol Readable;

__attribute__((objc_root_class))
@interface Earlier
- (int)value;
@end

@protocol Readable
- (int)read;
@end

__attribute__((objc_root_class))
@interface Box<__covariant Item>
@end
