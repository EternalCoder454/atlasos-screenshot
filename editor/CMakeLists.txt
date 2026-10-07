cmake_minimum_required(VERSION 3.24)
project(telamon-screenshot-editor VERSION 0.2.0 LANGUAGES CXX)

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)
if(NOT CMAKE_BUILD_TYPE AND NOT CMAKE_CONFIGURATION_TYPES)
    set(CMAKE_BUILD_TYPE Release)
endif()
set(CMAKE_CXX_FLAGS_RELEASE "-O2 -DNDEBUG")

include(GNUInstallDirs)

option(TELAMON_EDITOR_TESTS "Build the tests and the test-only executable with the screenshot hooks" OFF)

find_package(Qt6 6.8 REQUIRED COMPONENTS Core Gui Qml Quick QuickControls2)
qt_policy(SET QTP0001 NEW)
qt_policy(SET QTP0004 NEW)

# Telamon.Ui is installed like Kirigami (the telamon-ui package): QML imports
# it, and nothing links it. qmlcachegen and qmllint read its types from there.
if(NOT EXISTS "${QT6_INSTALL_PREFIX}/${QT6_INSTALL_QML}/Telamon/Ui/qmldir")
    message(FATAL_ERROR "Telamon.Ui is not installed: dnf install telamon-ui")
endif()

# Everything but main(): the C++ types and the compiled QML, as one static
# QML module, so the executable and the tests use the same code.
qt_add_library(editorcore STATIC)
qt_add_qml_module(editorcore
    URI net.eterneon.telamon.screenshoteditor
    VERSION 1.0
    OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/net/eterneon/telamon/screenshoteditor
    SOURCES
        src/render.h
        src/render.cpp
        src/backend.h
        src/backend.cpp
        src/canvasview.h
        src/canvasview.cpp
    QML_FILES
        qml/Main.qml
        qml/CanvasStage.qml
        qml/NewScreenshotPanel.qml
        qml/PathDialog.qml
        qml/Document.qml
)
target_link_libraries(editorcore PUBLIC Qt6::Core Qt6::Gui Qt6::Qml Qt6::Quick)
target_include_directories(editorcore PUBLIC ${CMAKE_CURRENT_SOURCE_DIR}/src)

# Telamon.Ui and Kirigami are shared plugins that Qt finds at run time. The
# scan of the imports (which looks for static plugins to link) has nothing to
# do here and only warns about them, so it is skipped for the executables.
set(EDITOR_LIBS editorcore editorcoreplugin Qt6::Core Qt6::Gui Qt6::Qml Qt6::Quick Qt6::QuickControls2)

qt_add_executable(telamon-screenshot-editor main.cpp)
target_compile_definitions(telamon-screenshot-editor PRIVATE TELAMON_EDITOR_VERSION="${PROJECT_VERSION}")
target_link_libraries(telamon-screenshot-editor PRIVATE ${EDITOR_LIBS})
set_target_properties(telamon-screenshot-editor PROPERTIES _QT_QML_PLUGINS_IMPORTED TRUE)

install(TARGETS telamon-screenshot-editor RUNTIME DESTINATION ${CMAKE_INSTALL_BINDIR})
install(FILES data/net.eterneon.telamon.screenshot.editor.desktop
        DESTINATION ${CMAKE_INSTALL_DATADIR}/applications)

if(TELAMON_EDITOR_TESTS)
    enable_testing()
    find_package(Qt6 6.8 REQUIRED COMPONENTS Test QuickTest)

    # The same main() with the hooks for screenshots and timing.
    qt_add_executable(telamon-screenshot-editor-test main.cpp)
    target_compile_definitions(telamon-screenshot-editor-test PRIVATE
        TELAMON_EDITOR_VERSION="${PROJECT_VERSION}" TELAMON_EDITOR_TEST_HOOKS=1)
    target_link_libraries(telamon-screenshot-editor-test PRIVATE ${EDITOR_LIBS})
    set_target_properties(telamon-screenshot-editor-test PROPERTIES _QT_QML_PLUGINS_IMPORTED TRUE)

    qt_add_executable(tst_backend tests/tst_backend.cpp)
    target_link_libraries(tst_backend PRIVATE ${EDITOR_LIBS} Qt6::Test)
    set_target_properties(tst_backend PROPERTIES _QT_QML_PLUGINS_IMPORTED TRUE)
    add_test(NAME backend COMMAND tst_backend)

    qt_add_executable(tst_qml tests/tst_qml.cpp)
    target_compile_definitions(tst_qml PRIVATE QUICK_TEST_SOURCE_DIR="${CMAKE_CURRENT_SOURCE_DIR}/tests")
    target_link_libraries(tst_qml PRIVATE ${EDITOR_LIBS} Qt6::QuickTest)
    set_target_properties(tst_qml PROPERTIES _QT_QML_PLUGINS_IMPORTED TRUE)
    add_test(NAME qml COMMAND tst_qml)

    foreach(t backend qml)
        set_tests_properties(${t} PROPERTIES
            ENVIRONMENT "QT_QPA_PLATFORM=offscreen;QT_QUICK_BACKEND=software;TELAMON_SCREENSHOT_BIN=${CMAKE_CURRENT_SOURCE_DIR}/tests/fake-telamon-screenshot.sh")
    endforeach()
    # Needs the fonts and the module of the image; skipped by name when not wanted.
endif()
