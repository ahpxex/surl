// surl 的 WPT 嵌入方钩子:testharness 完成后把结果放到全局,宿主读走。
// (每个 WPT 测试文件都会引 /resources/testharnessreport.js,内容由嵌入方定义)
setup({ output: false });
add_completion_callback(function (tests, harnessStatus) {
  globalThis.__wpt_results = tests.map(function (t) {
    return {
      name: t.name,
      // 0 PASS / 1 FAIL / 2 TIMEOUT / 3 NOTRUN / 4 PRECONDITION_FAILED
      status: t.status,
      message: t.message === null || t.message === undefined ? "" : String(t.message),
      stack: t.stack == null ? "" : String(t.stack),
    };
  });
  globalThis.__wpt_harness_status = harnessStatus.status;
  globalThis.__wpt_harness_message =
    harnessStatus.message == null ? "" : String(harnessStatus.message);
});
