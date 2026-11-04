/*
 * The image builds chdman alone, without SDL. MAME's utility library still names SDL's clipboard calls, which
 * chdman never makes; these stand in for them, so the binary needs nothing but the C library.
 */
#include <stdlib.h>

int SDL_HasClipboardText(void) { return 0; }
char *SDL_GetClipboardText(void) { return NULL; }
int SDL_SetClipboardText(const char *text) { (void)text; return -1; }
void SDL_free(void *mem) { free(mem); }
