/** Greets a café visitor. */
#include <string>

namespace demo {
class Visitor {
public:
    std::string greet(const std::string &name) {
        return decorate("olá", name);
    }
};
}
