/** Bounded transport storage; this never changes the native consumer clock. */
export const DEVICE_BUFFER_BLOCKS = 32;
// Start with 683–743 ms of PCM at the supported device rates, then keep
// filling the full queue. Startup/recovery need not wait for 2.7–3.0 seconds
// of audio to be produced before the device can consume its first sample.
export const DEVICE_PRIME_BLOCKS = 8;
