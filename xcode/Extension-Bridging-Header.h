//  Extension-Bridging-Header.h
//  Set as the extension target's "Objective-C Bridging Header" so Swift can call
//  the Rust entry point.

#import <FSKit/FSKit.h>

/// Exported by the fskit-s3-ext Rust staticlib. Returns an autoreleased
/// FSUnaryFileSystem subclass instance (FSKitS3FileSystem) that conforms to
/// FSUnaryFileSystemOperations.
FSUnaryFileSystem * _Nonnull fskit_s3_make_filesystem(void);
