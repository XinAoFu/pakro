project('p6-meson-native', 'c', version: '1.0.0')
cc = meson.get_compiler('c')
probe = cc.run('int main(void) { return 0; }', name: 'actual registered try_run')
if not probe.compiled() or probe.returncode() != 0
  error('the actual Meson test executable did not run')
endif
py = import('python').find_installation(pure: false)
py.extension_module('p6_native_demo', 'src/demo.c', install: true)
