//  FileSystemExtension.swift
//  The ExtensionKit @main bootstrap — the ONLY Swift in the project.
//
//  It contains no logic. ExtensionKit requires a Swift `@main` type conforming
//  to `UnaryFileSystemExtension`; its `fileSystem` property just returns our
//  Rust-defined `FSUnaryFileSystem` (built in the fskit-s3-ext staticlib and
//  exposed as `fskit_s3_make_filesystem`, declared in the bridging header).

import ExtensionFoundation
import FSKit

@main
struct FskitS3Extension: UnaryFileSystemExtension {
    var fileSystem: FSUnaryFileSystem & FSUnaryFileSystemOperations {
        // The Rust class (FSKitS3FileSystem) conforms to FSUnaryFileSystemOperations;
        // the cast is checked at runtime via the ObjC protocol conformance.
        fskit_s3_make_filesystem() as! (FSUnaryFileSystem & FSUnaryFileSystemOperations)
    }
}
