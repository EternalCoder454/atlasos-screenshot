// Process-level hardening, called first thing in main().
#pragma once

namespace shot {
// The editor holds screen pixels in memory, so it must never leave a core
// dump: sets RLIMIT_CORE to 0 (soft and hard, so nothing it starts can raise
// it either). Returns false when the limit could not be set.
//
// PR_SET_DUMPABLE is deliberately not used: a non-dumpable process makes
// /proc/<pid>/exe unreadable to KWin, which needs it to tell which program
// a window belongs to.
bool hardenProcess();
} // namespace shot
