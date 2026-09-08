# pi-media

Provider-neutral multimodal input processing. The first supported modality is images;
video and audio can gain their own modules when implemented.

`image::process_image(bytes, policy)` detects supported bytes and returns a `pi_core::ImageContent`
plus processing hints. `ImagePolicy::default()` uses Pi's 2000×2000 dimension bounds and an
exclusive 4,718,592-byte base64 limit. PNG, JPEG, GIF and WebP retain their original bytes when
within limits. BMP converts to PNG even when automatic resizing is disabled. Resizing uses the
existing Rust PNG/Lanczos implementation, so encoded bytes need not match Pi's TypeScript encoder.

`image::detect_mime_type` follows the local Pi oracle's magic-byte rules, including APNG and
JPEG-LS exclusions. `image::encode_rgba_png` converts native clipboard pixels without resizing.
Failures are typed `MediaError` values; callers decide whether to omit an attachment with a
notice or fail their operation.

The crate performs synchronous CPU work. Async callers schedule it on blocking workers. File
paths, clipboard access, temporary-file lifetime, terminal presentation, settings/trust decisions,
provider capability selection and wire serialization belong to callers. No filesystem or
provider-specific policy is hidden here.
