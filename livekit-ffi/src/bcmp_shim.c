#include <string.h>
int bcmp(const void *a, const void *b, size_t n) {
    return memcmp(a, b, n);
}
