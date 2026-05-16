/*
 * libnppicc-stub.c — no-op stubs for the 6 CUDA/NPP symbols used by
 * cuda_npp.rs, built into libnppicc-stub.so at dev-container image-build
 * time.  Only used when the real libnpp-dev-12-4 package is unavailable
 * (e.g. NVIDIA apt repo unreachable in CI).  Each function returns an error
 * code so the Rust constructor can detect the stub and surface a clear
 * "not on real hardware" message rather than crashing or hanging.
 *
 * Build (run by Dockerfile.dev, not committed as a binary):
 *   gcc -shared -fPIC -o tests/fixtures/libnppicc-stub.so \
 *       tests/fixtures/libnppicc-stub.c
 */

#include <stddef.h>
#include <stdint.h>

/* ---- CUDA driver API types (minimal subset) ---- */
typedef unsigned long long CUdeviceptr;
typedef void *CUstream;
typedef int CUresult;
typedef int NppStatus;

#define CUDA_ERROR_NOT_INITIALIZED 3
#define NPP_NOT_IMPLEMENTED_ERROR  -9999

/* Descriptor for a 2-D memory copy — layout matches CUDA_MEMCPY2D. */
typedef struct {
    size_t srcXInBytes;
    size_t srcY;
    int    srcMemoryType;
    const void *srcHost;
    CUdeviceptr srcDevice;
    void       *srcArray;
    size_t      srcPitch;
    size_t dstXInBytes;
    size_t dstY;
    int    dstMemoryType;
    void       *dstHost;
    CUdeviceptr dstDevice;
    void       *dstArray;
    size_t      dstPitch;
    size_t WidthInBytes;
    size_t Height;
} CUDA_MEMCPY2D;

/* NPP size struct. */
typedef struct { int width; int height; } NppiSize;

/* ---- Stub implementations ---- */

CUresult cuMemAlloc(CUdeviceptr *devPtr, size_t bytesize)
{
    (void)devPtr; (void)bytesize;
    return CUDA_ERROR_NOT_INITIALIZED;
}

CUresult cuMemFree(CUdeviceptr devPtr)
{
    (void)devPtr;
    return CUDA_ERROR_NOT_INITIALIZED;
}

CUresult cuMemcpy2D(const CUDA_MEMCPY2D *pCopy)
{
    (void)pCopy;
    return CUDA_ERROR_NOT_INITIALIZED;
}

CUresult cuMemcpy2DAsync(const CUDA_MEMCPY2D *pCopy, CUstream hStream)
{
    (void)pCopy; (void)hStream;
    return CUDA_ERROR_NOT_INITIALIZED;
}

CUresult cuStreamSynchronize(CUstream hStream)
{
    (void)hStream;
    return CUDA_ERROR_NOT_INITIALIZED;
}

NppStatus nppiBGRToYUV420_8u_AC4P3R(
    const uint8_t *pSrc,
    int            nSrcStep,
    uint8_t      **pDst,
    const int     *rDstStep,
    NppiSize       oSizeROI)
{
    (void)pSrc; (void)nSrcStep; (void)pDst; (void)rDstStep; (void)oSizeROI;
    return NPP_NOT_IMPLEMENTED_ERROR;
}
