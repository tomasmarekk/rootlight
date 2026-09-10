// Type parameter bindings are distinct from protocol and superclass arguments.
// Root classes, categories and forwards exercise ambiguous native node shapes.
@protocol Readable
@end

__attribute__((objc_root_class))
@interface Root
@end

@interface Container<__covariant Element : Root *> : Root <Readable>
- (Element)value;
@end

@interface Pair<Key, Value> : Root
- (Key)key;
- (Value)value;
@end

@interface Specialized : Container<Root *> <Readable>
@end

@interface Container<Element> (Extras)
- (Element)extra;
@end

@class Pending<__contravariant Input>, Later<Other>;

__attribute__((objc_root_class))
@interface Conforming <Readable>
@end

__attribute__((objc_root_class))
@interface Covariant<__covariant Item>
@end
