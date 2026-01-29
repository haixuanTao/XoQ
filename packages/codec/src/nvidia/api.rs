//! NVIDIA encoder API function pointer loading.
//!
//! This module provides a lazy-loaded interface to the NVENC API functions.

use std::ffi::{c_int, c_void};

use lazy_static::lazy_static;

use nvidia_sys::{
    NvEncodeAPICreateInstance, NvEncodeAPIGetMaxSupportedVersion,
    GUID, NVENCAPI_MAJOR_VERSION, NVENCAPI_MINOR_VERSION, NVENCSTATUS,
    NV_ENCODE_API_FUNCTION_LIST, NV_ENCODE_API_FUNCTION_LIST_VER,
    NV_ENC_BUFFER_FORMAT, NV_ENC_CAPS_PARAM, NV_ENC_CREATE_BITSTREAM_BUFFER,
    NV_ENC_CREATE_INPUT_BUFFER, NV_ENC_CUSTREAM_PTR, NV_ENC_INITIALIZE_PARAMS,
    NV_ENC_INPUT_PTR, NV_ENC_LOCK_BITSTREAM, NV_ENC_LOCK_INPUT_BUFFER,
    NV_ENC_MAP_INPUT_RESOURCE, NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS,
    NV_ENC_OUTPUT_PTR, NV_ENC_PIC_PARAMS, NV_ENC_PRESET_CONFIG,
    NV_ENC_RECONFIGURE_PARAMS, NV_ENC_REGISTERED_PTR, NV_ENC_REGISTER_RESOURCE,
    NV_ENC_TUNING_INFO,
};

use crate::CodecError;

lazy_static! {
    /// A lazy static for the Encoder API.
    pub static ref ENCODE_API: EncodeAPI = EncodeAPI::new();
}

// Function type aliases
type OpenEncodeSessionEx = unsafe extern "C" fn(
    *mut NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS,
    *mut *mut c_void,
) -> NVENCSTATUS;
type InitializeEncoder = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_INITIALIZE_PARAMS) -> NVENCSTATUS;
type DestroyEncoder = unsafe extern "C" fn(*mut c_void) -> NVENCSTATUS;
type GetEncodeGUIDCount = unsafe extern "C" fn(*mut c_void, *mut u32) -> NVENCSTATUS;
type GetEncodeGUIDs = unsafe extern "C" fn(*mut c_void, *mut GUID, u32, *mut u32) -> NVENCSTATUS;
type GetInputFormatCount = unsafe extern "C" fn(*mut c_void, GUID, *mut u32) -> NVENCSTATUS;
type GetInputFormats = unsafe extern "C" fn(*mut c_void, GUID, *mut NV_ENC_BUFFER_FORMAT, u32, *mut u32) -> NVENCSTATUS;
type GetEncodeCaps = unsafe extern "C" fn(*mut c_void, GUID, *mut NV_ENC_CAPS_PARAM, *mut c_int) -> NVENCSTATUS;
type GetEncodePresetCount = unsafe extern "C" fn(*mut c_void, GUID, *mut u32) -> NVENCSTATUS;
type GetEncodePresetGUIDs = unsafe extern "C" fn(*mut c_void, GUID, *mut GUID, u32, *mut u32) -> NVENCSTATUS;
type GetEncodePresetConfigEx = unsafe extern "C" fn(*mut c_void, GUID, GUID, NV_ENC_TUNING_INFO, *mut NV_ENC_PRESET_CONFIG) -> NVENCSTATUS;
type CreateInputBuffer = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_CREATE_INPUT_BUFFER) -> NVENCSTATUS;
type DestroyInputBuffer = unsafe extern "C" fn(*mut c_void, NV_ENC_INPUT_PTR) -> NVENCSTATUS;
type CreateBitstreamBuffer = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_CREATE_BITSTREAM_BUFFER) -> NVENCSTATUS;
type DestroyBitstreamBuffer = unsafe extern "C" fn(*mut c_void, NV_ENC_OUTPUT_PTR) -> NVENCSTATUS;
type EncodePicture = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_PIC_PARAMS) -> NVENCSTATUS;
type LockBitstream = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_LOCK_BITSTREAM) -> NVENCSTATUS;
type UnlockBitstream = unsafe extern "C" fn(*mut c_void, NV_ENC_OUTPUT_PTR) -> NVENCSTATUS;
type LockInputBuffer = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_LOCK_INPUT_BUFFER) -> NVENCSTATUS;
type UnlockInputBuffer = unsafe extern "C" fn(*mut c_void, NV_ENC_INPUT_PTR) -> NVENCSTATUS;
type MapInputResource = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_MAP_INPUT_RESOURCE) -> NVENCSTATUS;
type UnmapInputResource = unsafe extern "C" fn(*mut c_void, NV_ENC_INPUT_PTR) -> NVENCSTATUS;
type RegisterResource = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_REGISTER_RESOURCE) -> NVENCSTATUS;
type UnregisterResource = unsafe extern "C" fn(*mut c_void, NV_ENC_REGISTERED_PTR) -> NVENCSTATUS;
type ReconfigureEncoder = unsafe extern "C" fn(*mut c_void, *mut NV_ENC_RECONFIGURE_PARAMS) -> NVENCSTATUS;
type GetLastErrorString = unsafe extern "C" fn(*mut c_void) -> *const ::core::ffi::c_char;
type SetIOCudaStreams = unsafe extern "C" fn(*mut c_void, NV_ENC_CUSTREAM_PTR, NV_ENC_CUSTREAM_PTR) -> NVENCSTATUS;

/// NVENC API function pointers.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct EncodeAPI {
    pub open_encode_session_ex: OpenEncodeSessionEx,
    pub initialize_encoder: InitializeEncoder,
    pub destroy_encoder: DestroyEncoder,
    pub get_encode_guid_count: GetEncodeGUIDCount,
    pub get_encode_guids: GetEncodeGUIDs,
    pub get_input_format_count: GetInputFormatCount,
    pub get_input_formats: GetInputFormats,
    pub get_encode_caps: GetEncodeCaps,
    pub get_encode_preset_count: GetEncodePresetCount,
    pub get_encode_preset_guids: GetEncodePresetGUIDs,
    pub get_encode_preset_config_ex: GetEncodePresetConfigEx,
    pub create_input_buffer: CreateInputBuffer,
    pub destroy_input_buffer: DestroyInputBuffer,
    pub create_bitstream_buffer: CreateBitstreamBuffer,
    pub destroy_bitstream_buffer: DestroyBitstreamBuffer,
    pub encode_picture: EncodePicture,
    pub lock_bitstream: LockBitstream,
    pub unlock_bitstream: UnlockBitstream,
    pub lock_input_buffer: LockInputBuffer,
    pub unlock_input_buffer: UnlockInputBuffer,
    pub map_input_resource: MapInputResource,
    pub unmap_input_resource: UnmapInputResource,
    pub register_resource: RegisterResource,
    pub unregister_resource: UnregisterResource,
    pub reconfigure_encoder: ReconfigureEncoder,
    pub get_last_error_string: GetLastErrorString,
    pub set_io_cuda_streams: SetIOCudaStreams,
}

impl EncodeAPI {
    fn new() -> Self {
        const MSG: &str = "The API instance should populate the whole function list.";

        // Check that the driver max supported version matches
        let mut version = 0u32;
        unsafe { NvEncodeAPIGetMaxSupportedVersion(&mut version) }
            .result_without_string()
            .expect("Failed to get max supported NVENC version");

        let major_version = version >> 4;
        let minor_version = version & 0b1111;
        assert!(
            (major_version, minor_version) >= (NVENCAPI_MAJOR_VERSION, NVENCAPI_MINOR_VERSION),
            "NVENC driver version {}.{} is older than required {}.{}",
            major_version, minor_version,
            NVENCAPI_MAJOR_VERSION, NVENCAPI_MINOR_VERSION
        );

        // Create empty function buffer
        let mut function_list = NV_ENCODE_API_FUNCTION_LIST {
            version: NV_ENCODE_API_FUNCTION_LIST_VER,
            ..Default::default()
        };

        // Create Encode API Instance (populate function buffer)
        unsafe { NvEncodeAPICreateInstance(&mut function_list) }
            .result_without_string()
            .expect("Failed to create NVENC API instance");

        Self {
            open_encode_session_ex: function_list.nvEncOpenEncodeSessionEx.expect(MSG),
            initialize_encoder: function_list.nvEncInitializeEncoder.expect(MSG),
            destroy_encoder: function_list.nvEncDestroyEncoder.expect(MSG),
            get_encode_guid_count: function_list.nvEncGetEncodeGUIDCount.expect(MSG),
            get_encode_guids: function_list.nvEncGetEncodeGUIDs.expect(MSG),
            get_input_format_count: function_list.nvEncGetInputFormatCount.expect(MSG),
            get_input_formats: function_list.nvEncGetInputFormats.expect(MSG),
            get_encode_caps: function_list.nvEncGetEncodeCaps.expect(MSG),
            get_encode_preset_count: function_list.nvEncGetEncodePresetCount.expect(MSG),
            get_encode_preset_guids: function_list.nvEncGetEncodePresetGUIDs.expect(MSG),
            get_encode_preset_config_ex: function_list.nvEncGetEncodePresetConfigEx.expect(MSG),
            create_input_buffer: function_list.nvEncCreateInputBuffer.expect(MSG),
            destroy_input_buffer: function_list.nvEncDestroyInputBuffer.expect(MSG),
            create_bitstream_buffer: function_list.nvEncCreateBitstreamBuffer.expect(MSG),
            destroy_bitstream_buffer: function_list.nvEncDestroyBitstreamBuffer.expect(MSG),
            encode_picture: function_list.nvEncEncodePicture.expect(MSG),
            lock_bitstream: function_list.nvEncLockBitstream.expect(MSG),
            unlock_bitstream: function_list.nvEncUnlockBitstream.expect(MSG),
            lock_input_buffer: function_list.nvEncLockInputBuffer.expect(MSG),
            unlock_input_buffer: function_list.nvEncUnlockInputBuffer.expect(MSG),
            map_input_resource: function_list.nvEncMapInputResource.expect(MSG),
            unmap_input_resource: function_list.nvEncUnmapInputResource.expect(MSG),
            register_resource: function_list.nvEncRegisterResource.expect(MSG),
            unregister_resource: function_list.nvEncUnregisterResource.expect(MSG),
            reconfigure_encoder: function_list.nvEncReconfigureEncoder.expect(MSG),
            get_last_error_string: function_list.nvEncGetLastErrorString.expect(MSG),
            set_io_cuda_streams: function_list.nvEncSetIOCudaStreams.expect(MSG),
        }
    }
}

/// Extension trait for NVENCSTATUS to convert to Result.
pub trait NvencStatusExt {
    fn result(self, encoder_ptr: *mut c_void) -> Result<(), CodecError>;
    fn result_without_string(self) -> Result<(), CodecError>;
}

impl NvencStatusExt for NVENCSTATUS {
    fn result(self, encoder_ptr: *mut c_void) -> Result<(), CodecError> {
        self.result_without_string().map_err(|mut err| {
            // Try to get more detailed error message
            if !encoder_ptr.is_null() {
                let error_str = unsafe { (ENCODE_API.get_last_error_string)(encoder_ptr) };
                if !error_str.is_null() {
                    let c_str = unsafe { std::ffi::CStr::from_ptr(error_str) };
                    if let Ok(s) = c_str.to_str() {
                        if !s.is_empty() {
                            err = CodecError::Generic(format!("{}: {}", err, s));
                        }
                    }
                }
            }
            err
        })
    }

    fn result_without_string(self) -> Result<(), CodecError> {
        match self {
            NVENCSTATUS::NV_ENC_SUCCESS => Ok(()),
            NVENCSTATUS::NV_ENC_ERR_NO_ENCODE_DEVICE => Err(CodecError::NoEncodeDevice),
            NVENCSTATUS::NV_ENC_ERR_UNSUPPORTED_DEVICE => Err(CodecError::UnsupportedDevice),
            NVENCSTATUS::NV_ENC_ERR_INVALID_ENCODERDEVICE => Err(CodecError::InvalidEncoderDevice),
            NVENCSTATUS::NV_ENC_ERR_INVALID_DEVICE => Err(CodecError::InvalidDevice),
            NVENCSTATUS::NV_ENC_ERR_DEVICE_NOT_EXIST => Err(CodecError::DeviceNotExist),
            NVENCSTATUS::NV_ENC_ERR_INVALID_PTR => Err(CodecError::InvalidPtr),
            NVENCSTATUS::NV_ENC_ERR_INVALID_EVENT => Err(CodecError::Generic("invalid event".into())),
            NVENCSTATUS::NV_ENC_ERR_INVALID_PARAM => Err(CodecError::InvalidParam("invalid parameter".into())),
            NVENCSTATUS::NV_ENC_ERR_INVALID_CALL => Err(CodecError::InvalidCall),
            NVENCSTATUS::NV_ENC_ERR_OUT_OF_MEMORY => Err(CodecError::OutOfMemory),
            NVENCSTATUS::NV_ENC_ERR_ENCODER_NOT_INITIALIZED => Err(CodecError::EncoderNotInitialized),
            NVENCSTATUS::NV_ENC_ERR_UNSUPPORTED_PARAM => Err(CodecError::UnsupportedParam("unsupported parameter".into())),
            NVENCSTATUS::NV_ENC_ERR_LOCK_BUSY => Err(CodecError::LockBusy),
            NVENCSTATUS::NV_ENC_ERR_NOT_ENOUGH_BUFFER => Err(CodecError::NotEnoughBuffer),
            NVENCSTATUS::NV_ENC_ERR_INVALID_VERSION => Err(CodecError::InvalidVersion),
            NVENCSTATUS::NV_ENC_ERR_MAP_FAILED => Err(CodecError::MapFailed),
            NVENCSTATUS::NV_ENC_ERR_NEED_MORE_INPUT => Err(CodecError::NeedMoreInput),
            NVENCSTATUS::NV_ENC_ERR_ENCODER_BUSY => Err(CodecError::EncoderBusy),
            NVENCSTATUS::NV_ENC_ERR_EVENT_NOT_REGISTERD => Err(CodecError::Generic("event not registered".into())),
            NVENCSTATUS::NV_ENC_ERR_GENERIC => Err(CodecError::Generic("generic error".into())),
            NVENCSTATUS::NV_ENC_ERR_INCOMPATIBLE_CLIENT_KEY => Err(CodecError::Generic("incompatible client key".into())),
            NVENCSTATUS::NV_ENC_ERR_UNIMPLEMENTED => Err(CodecError::Unimplemented("feature not implemented".into())),
            NVENCSTATUS::NV_ENC_ERR_RESOURCE_REGISTER_FAILED => Err(CodecError::ResourceRegisterFailed),
            NVENCSTATUS::NV_ENC_ERR_RESOURCE_NOT_REGISTERED => Err(CodecError::ResourceNotRegistered),
            NVENCSTATUS::NV_ENC_ERR_RESOURCE_NOT_MAPPED => Err(CodecError::ResourceNotMapped),
            NVENCSTATUS::NV_ENC_ERR_NEED_MORE_OUTPUT => Err(CodecError::NeedMoreOutput),
        }
    }
}
