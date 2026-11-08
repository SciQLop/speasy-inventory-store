project('store_qt', 'cpp', default_options: ['cpp_std=c++20', 'buildtype=release'])

py = import('python').find_installation(get_option('python'), pure: false)
qt6 = dependency('qt6', modules: ['Core', 'Test'])

py.extension_module('store_qt', 'module.cpp', 'StoreModel.cpp', 'db_module.cpp', 'DbModel.cpp',
  include_directories: include_directories('../include'),
  dependencies: [qt6, py.dependency()],
  install: false)
