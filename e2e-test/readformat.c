// Preloaded into sway: makes its OpenGL renderer report a chosen pixel format
// as the one it reads fastest, which wlroots then offers for window capture,
// and reads pixels in that format by converting from RGBA, so the driver need
// not support it.
//
// E2E_FORMAT names the format, as in wlroots' table: xrgb8888, bgr888, ...
// While the file E2E_PAUSE names exists, the real format is reported, so that
// screenshots of the output are taken as usual.

#define _GNU_SOURCE
#include <dlfcn.h>
#include <math.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

typedef unsigned int GLenum;
typedef int GLint;
typedef int GLsizei;

#define GL_ALPHA_BITS 0x0D55
#define GL_IMPLEMENTATION_COLOR_READ_TYPE 0x8B9A
#define GL_IMPLEMENTATION_COLOR_READ_FORMAT 0x8B9B
#define GL_RGB 0x1907
#define GL_RGBA 0x1908
#define GL_BGRA_EXT 0x80E1
#define GL_UNSIGNED_BYTE 0x1401
#define GL_UNSIGNED_SHORT 0x1403
#define GL_UNSIGNED_SHORT_4_4_4_4 0x8033
#define GL_UNSIGNED_SHORT_5_5_5_1 0x8034
#define GL_UNSIGNED_SHORT_5_6_5 0x8363
#define GL_UNSIGNED_INT_2_10_10_10_REV 0x8368
#define GL_HALF_FLOAT_OES 0x8D61

struct format {
	const char *name;
	GLenum gl_format, gl_type;
	// wlroots picks the variant with or without alpha by this.
	GLint alpha_bits;
};

// Every entry of wlroots' GLES2 pixel format table (render/gles2/pixel_format.c).
static const struct format formats[] = {
	{"argb8888", GL_BGRA_EXT, GL_UNSIGNED_BYTE, 8},
	{"xrgb8888", GL_BGRA_EXT, GL_UNSIGNED_BYTE, 0},
	{"abgr8888", GL_RGBA, GL_UNSIGNED_BYTE, 8},
	{"xbgr8888", GL_RGBA, GL_UNSIGNED_BYTE, 0},
	{"bgr888", GL_RGB, GL_UNSIGNED_BYTE, 0},
	{"rgba4444", GL_RGBA, GL_UNSIGNED_SHORT_4_4_4_4, 4},
	{"rgbx4444", GL_RGBA, GL_UNSIGNED_SHORT_4_4_4_4, 0},
	{"rgba5551", GL_RGBA, GL_UNSIGNED_SHORT_5_5_5_1, 1},
	{"rgbx5551", GL_RGBA, GL_UNSIGNED_SHORT_5_5_5_1, 0},
	{"rgb565", GL_RGB, GL_UNSIGNED_SHORT_5_6_5, 0},
	{"abgr2101010", GL_RGBA, GL_UNSIGNED_INT_2_10_10_10_REV, 2},
	{"xbgr2101010", GL_RGBA, GL_UNSIGNED_INT_2_10_10_10_REV, 0},
	// wlroots 0.20 counts these two as having alpha (they are missing from its
	// list of opaque formats), so it only picks them with alpha bits.
	{"bgr161616f", GL_RGB, GL_HALF_FLOAT_OES, 16},
	{"abgr16161616f", GL_RGBA, GL_HALF_FLOAT_OES, 16},
	{"xbgr16161616f", GL_RGBA, GL_HALF_FLOAT_OES, 0},
	{"bgr161616", GL_RGB, GL_UNSIGNED_SHORT, 16},
	{"abgr16161616", GL_RGBA, GL_UNSIGNED_SHORT, 16},
	{"xbgr16161616", GL_RGBA, GL_UNSIGNED_SHORT, 0},
};

static const struct format *chosen(void) {
	const char *name = getenv("E2E_FORMAT");
	for (size_t i = 0; name && i < sizeof(formats) / sizeof(formats[0]); i++) {
		if (strcmp(formats[i].name, name) == 0) {
			return &formats[i];
		}
	}
	return NULL;
}

static int paused(void) {
	const char *path = getenv("E2E_PAUSE");
	return path && access(path, F_OK) == 0;
}

void glGetIntegerv(GLenum pname, GLint *data) {
	static void (*real)(GLenum, GLint *);
	if (!real) {
		real = (void (*)(GLenum, GLint *))dlsym(RTLD_NEXT, "glGetIntegerv");
	}
	real(pname, data);
	const struct format *f = chosen();
	if (!f || paused()) {
		return;
	}
	switch (pname) {
	case GL_IMPLEMENTATION_COLOR_READ_FORMAT:
		*data = (GLint)f->gl_format;
		break;
	case GL_IMPLEMENTATION_COLOR_READ_TYPE:
		*data = (GLint)f->gl_type;
		break;
	case GL_ALPHA_BITS:
		*data = f->alpha_bits;
		break;
	}
}

static uint16_t half(float v) {
	if (v <= 0.0f) {
		return 0;
	}
	int e;
	float m = frexpf(v, &e); // v = m * 2^e, m in [0.5, 1)
	if (e - 1 < -14) {
		return 0; // too small; nothing here is this dark but black
	}
	int exponent = e - 1 + 15, mantissa = (int)lroundf((m * 2.0f - 1.0f) * 1024.0f);
	if (mantissa == 1024) { // rounded up into the next power of two
		mantissa = 0, exponent++;
	}
	return (uint16_t)(exponent << 10 | mantissa);
}

static unsigned scale(uint8_t c, unsigned max) {
	return (c * max + 127) / 255;
}

// Writes one RGBA8 pixel as `format`/`type`, the way GL packs it, and returns
// the bytes written.
static size_t pack(const uint8_t *p, GLenum format, GLenum type, uint8_t *out) {
	uint8_t r = p[0], g = p[1], b = p[2], a = p[3];
	int n = format == GL_RGB ? 3 : 4;
	uint16_t s;
	uint32_t u;
	switch (type) {
	case GL_UNSIGNED_BYTE:
		if (format == GL_BGRA_EXT) {
			out[0] = b, out[1] = g, out[2] = r, out[3] = a;
		} else {
			memcpy(out, p, n);
		}
		return n;
	case GL_UNSIGNED_SHORT_4_4_4_4:
		s = scale(r, 15) << 12 | scale(g, 15) << 8 | scale(b, 15) << 4 | scale(a, 15);
		memcpy(out, &s, 2);
		return 2;
	case GL_UNSIGNED_SHORT_5_5_5_1:
		s = scale(r, 31) << 11 | scale(g, 31) << 6 | scale(b, 31) << 1 | (a >> 7);
		memcpy(out, &s, 2);
		return 2;
	case GL_UNSIGNED_SHORT_5_6_5:
		s = scale(r, 31) << 11 | scale(g, 63) << 5 | scale(b, 31);
		memcpy(out, &s, 2);
		return 2;
	case GL_UNSIGNED_INT_2_10_10_10_REV:
		u = (uint32_t)scale(a, 3) << 30 | scale(b, 1023) << 20 | scale(g, 1023) << 10 | scale(r, 1023);
		memcpy(out, &u, 4);
		return 4;
	case GL_HALF_FLOAT_OES:
		for (int i = 0; i < n; i++) {
			s = half(p[i] / 255.0f);
			memcpy(out + 2 * i, &s, 2);
		}
		return 2 * n;
	case GL_UNSIGNED_SHORT:
		for (int i = 0; i < n; i++) {
			s = p[i] * 257;
			memcpy(out + 2 * i, &s, 2);
		}
		return 2 * n;
	}
	return 0;
}

// wlroots sets GL_PACK_ALIGNMENT to 1 before reading, so rows are packed tight.
void glReadPixels(GLint x, GLint y, GLsizei w, GLsizei h, GLenum format, GLenum type, void *pixels) {
	static void (*real)(GLint, GLint, GLsizei, GLsizei, GLenum, GLenum, void *);
	if (!real) {
		real = (void (*)(GLint, GLint, GLsizei, GLsizei, GLenum, GLenum, void *))dlsym(RTLD_NEXT, "glReadPixels");
	}
	if (format == GL_RGBA && type == GL_UNSIGNED_BYTE) {
		real(x, y, w, h, format, type, pixels);
		return;
	}
	uint8_t *rgba = malloc((size_t)w * h * 4);
	if (!rgba) {
		return;
	}
	real(x, y, w, h, GL_RGBA, GL_UNSIGNED_BYTE, rgba);
	uint8_t *out = pixels;
	for (size_t i = 0; i < (size_t)w * h; i++) {
		out += pack(rgba + 4 * i, format, type, out);
	}
	free(rgba);
}
