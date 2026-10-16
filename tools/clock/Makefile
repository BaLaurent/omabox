CFLAGS ?= -O2 -Wall -Wextra
# (libc declares the pointers it takes nonnull; the library checks them anyway, as libc does.)
omabox-clock.so: clock.c ../common/box.h
	$(CC) $(CFLAGS) $(CPPFLAGS) -fPIC -shared -fno-delete-null-pointer-checks -Wno-nonnull-compare $(LDFLAGS) -o $@ clock.c
clean:
	rm -f omabox-clock.so
.PHONY: clean
