/*
 * libnppicc-stub.c — no-op stubs for the 8 CUDA runtime API + NPP symbols
 * used by cuda_npp.rs (P2.5 implementation deviation: runtime API instead of
 * driver API — see cuda_npp.rs module doc).  Built into libnppicc-stub.so at
 * dev-container image-build time.  Only used when the real libnpp-dev-12-4
 * package is unavailable (e.g. NVIDIA apt repo unreachable in CI).  Each
 * function returns an error code so the Rust constructor can detect the stub
 * and surface a clear "not on real hardware" message rather than crashing.
 *
 * Build (run by Dockerfile.dev, not committed as a binary):
 *   gcc -shared -fPIC -o tests/fixtures/libnppicc-stub.so \
 *       tests/fixtures/libnppicc-stub.c
 *
 * Symbol inventory (must match cuda_npp.rs extern "C" block — currently 8):
 *   cudaMalloc, cudaFree, cudaMemcpy2D, cudaMemcpy2DAsync,
 *   cudaStreamSynchronize, cudaDriverGetVersion,
 *   nppiBGRToYUV420_8u_AC4P3R, nppiYCbCr420_8u_P3P2R
 */

#include <stddef.h>
#include <stdint.h>

/* ---- CUDA runtime API types (minimal subset) ---- */
typedef void   *cudaStream_t;
typedef int     cudaError_t;
typedef int     NppStatus;

/* cudaMemcpyKind — only the numeric values matter for the stub. */
typedef int cudaMemcpyKind;

#define cudaSuccess              0
#define cudaErrorNoDevice        100  /* CUDA_ERROR_NO_DEVICE */
#define NPP_NOT_IMPLEMENTED_ERROR (-9999)

/* NPP size struct. */
typedef struct { int width; int height; } NppiSize;

/* ---- Stub implementations (CUDA Runtime API) ---- */

cudaError_t cudaMalloc(void **devPtr, size_t size)
{
    (void)devPtr; (void)size;
    return cudaErrorNoDevice;
}

cudaError_t cudaFree(void *devPtr)
{
    (void)devPtr;
    return cudaErrorNoDevice;
}

cudaError_t cudaMemcpy2D(
    void *dst, size_t dpitch,
    const void *src, size_t spitch,
    size_t width, size_t height,
    cudaMemcpyKind kind)
{
    (void)dst; (void)dpitch; (void)src; (void)spitch;
    (void)width; (void)height; (void)kind;
    return cudaErrorNoDevice;
}

cudaError_t cudaMemcpy2DAsync(
    void *dst, size_t dpitch,
    const void *src, size_t spitch,
    size_t width, size_t height,
    cudaMemcpyKind kind, cudaStream_t stream)
{
    (void)dst; (void)dpitch; (void)src; (void)spitch;
    (void)width; (void)height; (void)kind; (void)stream;
    return cudaErrorNoDevice;
}

cudaError_t cudaStreamSynchronize(cudaStream_t stream)
{
    (void)stream;
    return cudaErrorNoDevice;
}

cudaError_t cudaDriverGetVersion(int *driverVersion)
{
    if (driverVersion) *driverVersion = 0;
    return cudaSuccess;  /* return success but version=0 so caller rejects < MIN */
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

NppStatus nppiYCbCr420_8u_P3P2R(
    const uint8_t * const *pSrc,
    const int             *rSrcStep,
    uint8_t               *pDstY,
    int                    nDstYStep,
    uint8_t               *pDstCbCr,
    int                    nDstCbCrStep,
    NppiSize               oSizeROI)
{
    (void)pSrc; (void)rSrcStep; (void)pDstY; (void)nDstYStep;
    (void)pDstCbCr; (void)nDstCbCrStep; (void)oSizeROI;
    return NPP_NOT_IMPLEMENTED_ERROR;
}
