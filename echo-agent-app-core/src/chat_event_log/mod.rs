include!("event.rs");
include!("journal.rs");
include!("projection.rs");
include!("usage.rs");
include!("presentation.rs");
#[cfg(any(test, feature = "test-utils"))]
#[path = "presentation_tests.rs"]
mod presentation_tests;
#[cfg(any(test, feature = "test-utils"))]
#[path = "usage_tests.rs"]
mod usage_tests;
include!("tests.rs");

#[cfg(feature = "test-utils")]
pub(crate) use presentation_tests::native_presentation_retention_case as run_native_presentation_retention_case;
#[cfg(feature = "test-utils")]
pub(crate) use usage_tests::native_usage_retention_case as run_native_usage_retention_case;
