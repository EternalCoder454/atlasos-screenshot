#include "harden.h"

#include <sys/resource.h>

namespace shot {

bool hardenProcess()
{
    const rlimit none{0, 0};
    return ::setrlimit(RLIMIT_CORE, &none) == 0;
}

} // namespace shot
