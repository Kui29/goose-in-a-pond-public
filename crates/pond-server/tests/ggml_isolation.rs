//! Whisper and llama.cpp each link their own GGML, at different revisions.

/// One shared symbol hands one engine the other's GGML, whose operation numbers differ, and
/// llama.cpp aborts building its first graph.
#[test]
fn whisper_and_llama_keep_separate_ggml() {
    let llama = llama_cpp_sys_2::ggml_flash_attn_ext as *const ();
    let whisper = whisper_rs_sys::ggml_flash_attn_ext as *const ();
    assert_ne!(llama, whisper);
}
