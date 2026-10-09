program demo
   integer :: endif, if, elseif, error
   integer, DIMENSION(2) :: function
   endif = 3; if = 2
   ! <- variable
   error = 1
   ! <- variable
   if (endif == 2) then
        ! <- variable
      endif = 5
      ! <- variable
      elseif = if + endif
        ! <- variable
        !      ^ variable
        !            ^ variable
   elseif (endif == 3) then
      function(if) = endif/elseif
      ! <- variable
      !               ^ variable
      !                     ^ variable
      print *, endif
   endif
end program

subroutine foo
  integer :: if(1), elseif, else
  character(len=32) :: format
  character(len=1) :: width
  format(7:9) = width//'.'//width
  ! <- variable
  if(1) = 5
  ! <- variable
end subroutine foo
