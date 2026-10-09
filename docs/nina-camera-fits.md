# Camera names in NINA FITS files

Regain reports the selected camera model as the imaging instrument. For an
ASI585MM Pro, NINA saves:

```text
INSTRUME= 'ZWO ASI585MM Pro'   / Imaging instrument name
```

This applies to the native **PulsarFab regain Retryable Camera** plugin, native
Windows ASCOM cameras, and Alpaca camera discovery. Setup labels and ASCOM slot
numbers identify connections; they do not replace the model in image metadata.
Stable NINA selection IDs, ASCOM ProgIDs, and Alpaca device IDs/numbers are
unchanged. A native plugin image still uses `CAMERAID = 'ZwoGain'` for compatibility.

NINA caches Alpaca discovery names. After changing a slot to another camera
model, refresh the camera chooser and reconnect before capturing images.

## Verification

On 2026-10-09, installed NINA 3.2.0.9001 loaded the corrected native plugin and
saved two physical ASI585MM Pro exposures through its ordinary imaging screen
and FITS writer. Both were 0.1 seconds, 3840 × 2160, RAW16, bin 1, with cooling
off. SDK mode used ZWO SDK 1.41; Direct USB mode had SDK fallback disabled.
Both files contain `INSTRUME = 'ZWO ASI585MM Pro'`.

| Capture | FITS SHA-256 |
| --- | --- |
| SDK | `e22728f74ea8f5049aeb307ae5df9f61bd2aa5ed8f0ffe945fb5d13e8b20acf3` |
| Direct USB | `3cb3475f1fd0a303785711481431d1ce4902824876f010ca1f3d7f2233292a2f` |

The regression tests exercise capture metadata, NINA's `CameraInfo` projection,
and its FITS header serializer for ASI585MM Pro, ASI2600MM Duo, ASI2600MM Pro,
ASI220MM Mini, ASI6200MM Pro, and the SDK simulator. Before the fix, four cases
failed because setup labels changed the model names, including Duo → Pro.
All six pass with the fix.

The native COM tests check connected `Name == SensorName` in all four slots
from both 32-bit and 64-bit clients. HTTP tests check the model name in Alpaca
discovery and camera properties, while preserving custom setup labels and
device IDs. Those adapter checks use simulated devices; the two physical
FITS captures above use the native NINA plugin.
