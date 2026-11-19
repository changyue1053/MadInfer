"""Generate the dependency-free binary-consumer Xcode project (Python stdlib only)."""
import json
from pathlib import Path

root = Path(__file__).resolve().parent
objects = {}
def ident(n): return f'{n:024X}'
def add(n, isa, **kw):
    objects[ident(n)] = dict(isa=isa, **kw)
    return ident(n)
def ref(n): return ident(n)
add(1, 'PBXProject', attributes={'BuildIndependentTargetsInParallel':'YES', 'LastUpgradeCheck':'1600', 'TargetAttributes':{ref(20):{'CreatedOnToolsVersion':'16.0'},ref(21):{'CreatedOnToolsVersion':'16.0','TestTargetID':ref(20)}}}, buildConfigurationList=ref(50), compatibilityVersion='Xcode 14.0', developmentRegion='en', hasScannedForEncodings='0', knownRegions=['en','Base'], mainGroup=ref(2), productRefGroup=ref(3), projectDirPath='', projectRoot='', targets=[ref(20),ref(21)])
add(2,'PBXGroup',children=[ref(4),ref(5),ref(6),ref(3)],sourceTree='<group>')
add(3,'PBXGroup',name='Products',children=[ref(10),ref(11)],sourceTree='<group>')
add(4,'PBXGroup',path='App',children=[ref(7)],sourceTree='<group>')
add(5,'PBXGroup',path='Tests',children=[ref(8),ref(17),ref(18)],sourceTree='<group>')
add(6,'PBXFileReference',lastKnownFileType='wrapper.xcframework',name='MadInfer.xcframework',path='../../build/ios/MadInfer.xcframework',sourceTree='SOURCE_ROOT')
add(7,'PBXFileReference',lastKnownFileType='sourcecode.swift',path='AppDelegate.swift',sourceTree='<group>')
add(8,'PBXFileReference',lastKnownFileType='sourcecode.swift',path='MadInferSmokeTests.swift',sourceTree='<group>')
add(17,'PBXFileReference',lastKnownFileType='sourcecode.c.objc',path='NativeChecks.m',sourceTree='<group>')
add(18,'PBXFileReference',lastKnownFileType='sourcecode.c.h',path='NativeChecks.h',sourceTree='<group>')
add(19,'PBXBuildFile',fileRef=ref(17))
add(10,'PBXFileReference',explicitFileType='wrapper.application',includeInIndex='0',path='MadInferSmoke.app',sourceTree='BUILT_PRODUCTS_DIR')
add(11,'PBXFileReference',explicitFileType='wrapper.cfbundle',includeInIndex='0',path='MadInferSmokeTests.xctest',sourceTree='BUILT_PRODUCTS_DIR')
for n,file in [(12,7),(13,8),(14,6),(15,6)]: add(n,'PBXBuildFile',fileRef=ref(file))
add(16,'PBXBuildFile',fileRef=ref(6),settings={'ATTRIBUTES':['CodeSignOnCopy','RemoveHeadersOnCopy']})
add(20,'PBXNativeTarget',buildConfigurationList=ref(51),buildPhases=[ref(30),ref(31),ref(32),ref(33)],buildRules=[],dependencies=[],name='MadInferSmoke',productName='MadInferSmoke',productReference=ref(10),productType='com.apple.product-type.application')
add(21,'PBXNativeTarget',buildConfigurationList=ref(52),buildPhases=[ref(34),ref(35),ref(36)],buildRules=[],dependencies=[ref(40)],name='MadInferSmokeTests',productName='MadInferSmokeTests',productReference=ref(11),productType='com.apple.product-type.bundle.unit-test')
for n,isa,files in [(30,'PBXSourcesBuildPhase',[12]),(31,'PBXFrameworksBuildPhase',[14]),(32,'PBXResourcesBuildPhase',[]),(34,'PBXSourcesBuildPhase',[13,19]),(35,'PBXFrameworksBuildPhase',[15]),(36,'PBXResourcesBuildPhase',[])]:
    add(n,isa,buildActionMask='2147483647',files=list(map(ref,files)),runOnlyForDeploymentPostprocessing='0')
add(33,'PBXCopyFilesBuildPhase',buildActionMask='2147483647',dstPath='',dstSubfolderSpec='10',files=[ref(16)],name='Embed Frameworks',runOnlyForDeploymentPostprocessing='0')
add(40,'PBXTargetDependency',target=ref(20),targetProxy=ref(41))
add(41,'PBXContainerItemProxy',containerPortal=ref(1),proxyType='1',remoteGlobalIDString=ref(20),remoteInfo='MadInferSmoke')
for n,configs in [(50,[60,61]),(51,[62,63]),(52,[64,65])]:
    add(n,'XCConfigurationList',buildConfigurations=list(map(ref,configs)),defaultConfigurationIsVisible='0',defaultConfigurationName='Release')
base={'CLANG_ENABLE_MODULES':'YES','CLANG_ENABLE_OBJC_ARC':'YES','IPHONEOS_DEPLOYMENT_TARGET':'16.0','ARCHS':'arm64','ONLY_ACTIVE_ARCH':'YES','SDKROOT':'iphoneos','SWIFT_VERSION':'5.0','SUPPORTED_PLATFORMS':'iphoneos iphonesimulator','TARGETED_DEVICE_FAMILY':'1,2','CODE_SIGNING_ALLOWED':'NO'}
for n,name in [(60,'Debug'),(61,'Release')]:
    settings=dict(base,DEBUG_INFORMATION_FORMAT='dwarf',SWIFT_OPTIMIZATION_LEVEL='-Onone' if name=='Debug' else '-O')
    if name=='Debug': settings.update(ENABLE_TESTABILITY='YES',SWIFT_ACTIVE_COMPILATION_CONDITIONS='DEBUG')
    add(n,'XCBuildConfiguration',name=name,buildSettings=settings)
app={'PRODUCT_BUNDLE_IDENTIFIER':'org.example.madinfer.binarysmoke','PRODUCT_NAME':'$(TARGET_NAME)','GENERATE_INFOPLIST_FILE':'YES','INFOPLIST_KEY_UILaunchScreen_Generation':'YES','LD_RUNPATH_SEARCH_PATHS':['$(inherited)','@executable_path/Frameworks'],'SWIFT_EMIT_LOC_STRINGS':'NO'}
tests={'SWIFT_OBJC_BRIDGING_HEADER':'$(PROJECT_DIR)/Tests/NativeChecks.h','PRODUCT_BUNDLE_IDENTIFIER':'org.example.madinfer.binarysmoke.tests','PRODUCT_NAME':'$(TARGET_NAME)','GENERATE_INFOPLIST_FILE':'YES','TEST_HOST':'$(BUILT_PRODUCTS_DIR)/MadInferSmoke.app/MadInferSmoke','BUNDLE_LOADER':'$(TEST_HOST)','LD_RUNPATH_SEARCH_PATHS':['$(inherited)','@executable_path/Frameworks','@loader_path/Frameworks']}
for n,name,settings in [(62,'Debug',app),(63,'Release',app),(64,'Debug',tests),(65,'Release',tests)]: add(n,'XCBuildConfiguration',name=name,buildSettings=settings)
def emit(value,level=0):
    pad='\t'*level
    if isinstance(value,dict):
        return '{\n'+''.join('\t'*(level+1)+json.dumps(k)+' = '+emit(v,level+1)+';\n' for k,v in value.items())+pad+'}'
    if isinstance(value,list): return '('+', '.join(emit(v,level) for v in value)+')'
    return json.dumps(value)
project=dict(archiveVersion='1',classes={},objectVersion='56',objects=objects,rootObject=ref(1))
(root/'MadInferSmoke.xcodeproj/project.pbxproj').write_text('// !$*UTF8*$!\n'+emit(project)+'\n')
