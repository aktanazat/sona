import CoreMediaIO
import Foundation

let cameraProvider = SonaCameraProvider()
CMIOExtensionProvider.startService(provider: cameraProvider.provider)
CFRunLoopRun()
