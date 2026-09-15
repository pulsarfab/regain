/* Test the Rust FFI against the actual C header on each Unix ABI. No USB I/O. */
#include "../../vendor/zwo/ASICamera2.h"
#include <string.h>

static long gain = 100;
int ASIGetNumOfConnectedCameras(void) { return 1; }
ASI_ERROR_CODE ASIGetCameraProperty(ASI_CAMERA_INFO *info, int index) {
    (void)index;
    memset(info, 0, sizeof(*info));
    strcpy(info->Name, "Portable ABI fixture");
    info->CameraID = 7;
    info->MaxWidth = 9576;
    info->MaxHeight = 6388;
    info->PixelSize = 3.76;
    info->BitDepth = 16;
    info->IsCoolerCam = ASI_TRUE;
    info->SupportedBins[0] = 1;
    info->SupportedVideoFormat[0] = ASI_IMG_RAW16;
    info->SupportedVideoFormat[1] = ASI_IMG_END;
    return ASI_SUCCESS;
}
ASI_ERROR_CODE ASIOpenCamera(int id) { return id == 7 ? ASI_SUCCESS : ASI_ERROR_INVALID_ID; }
ASI_ERROR_CODE ASIInitCamera(int id) { return ASIOpenCamera(id); }
ASI_ERROR_CODE ASICloseCamera(int id) { return ASIOpenCamera(id); }
ASI_ERROR_CODE ASIGetSerialNumber(int id, ASI_ID *serial) {
    memset(serial->id, 7, sizeof(serial->id));
    return ASIOpenCamera(id);
}
ASI_ERROR_CODE ASIGetNumOfControls(int id, int *count) {
    *count = 1;
    return ASIOpenCamera(id);
}
ASI_ERROR_CODE ASIGetControlCaps(int id, int index, ASI_CONTROL_CAPS *caps) {
    (void)index;
    memset(caps, 0, sizeof(*caps));
    strcpy(caps->Name, "Gain");
    caps->MinValue = -123;
    caps->MaxValue = 700;
    caps->DefaultValue = 100;
    caps->IsWritable = ASI_TRUE;
    caps->ControlType = ASI_GAIN;
    return ASIOpenCamera(id);
}
ASI_ERROR_CODE ASIGetControlValue(int id, ASI_CONTROL_TYPE control, long *value, ASI_BOOL *automatic) {
    (void)control;
    *value = gain;
    *automatic = ASI_FALSE;
    return ASIOpenCamera(id);
}
ASI_ERROR_CODE ASISetControlValue(int id, ASI_CONTROL_TYPE control, long value, ASI_BOOL automatic) {
    (void)control;
    (void)automatic;
    gain = value;
    return ASIOpenCamera(id);
}
char *ASIGetSDKVersion(void) { return "C ABI fixture"; }
