MODULE BoundingBox_Method
  ! <- module
  !     ^ variable
  use other_module, only : foo
  ! <- keyword
  !     ^ variable
  !                  ^ keyword
  !                         ^ variable
  use, intrinsic :: iso_c_binding, only : real64
  ! <- keyword
  !     ^ keyword
  !                  ^ variable

  PRIVATE
  ! <- keyword
  public :: initiate_1, constructor_2
  ! <- keyword
  !           ^ variable
  INTERFACE
    ! <- keyword
    MODULE PURE SUBROUTINE initiate_1(obj, nsd, lim)
      ! <- attribute
      !     ^ attribute
      !          ^ keyword
      !                     ^ function
      !                                ^ variable.parameter
      CLASS(BoundingBox_), INTENT(INOUT) :: obj
      ! <- type
      !      ^ type
      !                     ^ attribute
      !                            ^ attribute
      !                                     ^ variable
    END SUBROUTINE initiate_1
    ! ^ keyword
  END INTERFACE

  INTERFACE
    MODULE FUNCTION Constructor_2(Anotherobj) RESULT(Ans)
      CLASS(BoundingBox_), POINTER :: Ans
      ! <- type
      !      ^ type
      !                     ^ attribute
    END FUNCTION Constructor_2
  END INTERFACE
END MODULE BoundingBox_Method
